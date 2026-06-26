use ash::vk;
use ash::{Device, Entry, Instance};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

pub struct VulkanDmaBufBackend {
    // Kept for lifetime — needed in Drop for device/instance cleanup.
    #[allow(dead_code)]
    entry: Entry,
    #[allow(dead_code)]
    physical_device: vk::PhysicalDevice,
    instance: Instance,
    device: Device,
    queue: vk::Queue,
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    fence: vk::Fence,
    // VkImage cache keyed by raw PipeWire fd (stable per buffer slot, no dup).
    // WARNING: PipeWire closes dma-buf on buffer renegotiation — cache isn't
    // invalidated automatically. Safe for fixed-resolution DBD use.
    image_cache: HashMap<RawFd, ImportedFrame>,
    // Crop geometry + CPU buffer — external code calls capture_crop() once
    // instead of orchestrating import/extract/map/unmap manually.
    crop_offset_x: i32,
    crop_offset_y: i32,
    crop_w: u32,
    crop_h: u32,
    cpu_buffer: vk::Buffer,
    cpu_memory: vk::DeviceMemory,
    /// Persistently mapped pointer (valid until unmap in Drop). HOST_COHERENT
    /// means GPU writes are visible without manual flush.
    mapped_ptr: *mut u8,
}

pub struct ImportedFrame {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
}

impl VulkanDmaBufBackend {
    /// Init headless Vulkan context with DMA-BUF + Nvidia modifier support.
    /// Creates the CPU crop buffer at startup.
    pub fn new(
        crop_offset_x: i32,
        crop_offset_y: i32,
        crop_w: u32,
        crop_h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let entry = unsafe { Entry::load() }?;

        let app_name = c"DbdBot";
        let app_info = vk::ApplicationInfo::builder()
            .application_name(app_name)
            .api_version(vk::make_api_version(0, 1, 3, 0));

        let instance_create_info = vk::InstanceCreateInfo::builder().application_info(&app_info);
        let instance = unsafe { entry.create_instance(&instance_create_info, None) }?;

        let pdevices = unsafe { instance.enumerate_physical_devices() }?;
        let physical_device = pdevices
            .into_iter()
            .find(|&p| {
                let props = unsafe { instance.get_physical_device_properties(p) };
                props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
            })
            .expect("Discrete GPU (Nvidia) not found!");

        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let queue_family_index = queue_families
            .iter()
            .position(|f| {
                f.queue_flags
                    .contains(vk::QueueFlags::TRANSFER | vk::QueueFlags::GRAPHICS)
            })
            .expect("Suitable queue not found") as u32;

        let priorities = [1.0];
        let queue_info = vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&priorities);

        let device_extensions = [
            ash::extensions::khr::ExternalMemoryFd::name().as_ptr(),
            vk::ExtImageDrmFormatModifierFn::name().as_ptr(),
            vk::KhrExternalMemoryFn::name().as_ptr(),
            vk::ExtQueueFamilyForeignFn::name().as_ptr(),
        ];

        let device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&device_extensions);

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }?;
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&pool_info, None)? };

        // Allocate a single command buffer to reuse
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info)? };
        let command_buffer = command_buffers[0];

        // Allocate a single fence to reuse
        let fence_info = vk::FenceCreateInfo::builder();
        let fence = unsafe { device.create_fence(&fence_info, None)? };

        // CPU crop buffer created once at startup.
        let (cpu_buffer, cpu_memory) =
            Self::create_cpu_buffer_impl(&device, &memory_properties, crop_w, crop_h)?;

        // Persistent map — done once here, unmapped in Drop.
        // HOST_VISIBLE | HOST_COHERENT: GPU writes are automatically visible.
        let mapped_ptr = unsafe {
            device.map_memory(
                cpu_memory,
                0,
                (crop_w * crop_h * 4) as u64,
                vk::MemoryMapFlags::empty(),
            )?
        } as *mut u8;

        Ok(Self {
            entry,
            instance,
            physical_device,
            device,
            queue,
            queue_family_index,
            command_pool,
            command_buffer,
            memory_properties,
            fence,
            image_cache: HashMap::new(),
            crop_offset_x,
            crop_offset_y,
            crop_w,
            crop_h,
            cpu_buffer,
            cpu_memory,
            mapped_ptr,
        })
    }

    /// Find memory type matching filter + properties (VRAM or RAM).
    fn find_memory_type(
        &self,
        type_filter: u32,
        properties: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        find_memory_type_in(&self.memory_properties, type_filter, properties)
    }

    /// Create VkImage + import dma-buf fd into it.
    ///
    /// Takes ownership of `fd_for_import` (an `OwnedFd`, normally a dup of the
    /// PipeWire fd). Per the `VK_KHR_external_memory_fd` spec, a *successful*
    /// import with `DMA_BUF_EXT` transfers fd ownership to the Vulkan
    /// implementation, which closes it when the memory object is freed — so we
    /// must NOT close it ourselves afterwards. On any failure before the fd is
    /// handed over, the `OwnedFd` drops and closes it for us. We also destroy
    /// any Vulkan objects already created on the error path.
    fn import_image_raw(
        &self,
        fd_for_import: OwnedFd,
        width: u32,
        height: u32,
        modifier: u64,
        stride: u32,
    ) -> Result<ImportedFrame, vk::Result> {
        // Set DRM modifier from PipeWire.
        let binding = [vk::SubresourceLayout::builder()
            .offset(0)
            .row_pitch(stride as u64)
            .size((stride * height) as u64)
            .build()];
        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::builder()
            .drm_format_modifier(modifier)
            .plane_layouts(&binding);

        // External memory (DMA-BUF).
        let mut external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM) // PipeWire BGRx → B8G8R8A8 in Vulkan
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC) // We copy FROM this image
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_info)
            .push_next(&mut modifier_info);

        let image = unsafe { self.device.create_image(&image_info, None)? };

        // Get memory requirements.
        let mem_reqs = unsafe { self.device.get_image_memory_requirements(image) };
        let mem_type_index = self
            .find_memory_type(
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .unwrap_or(0);

        // Import the fd into DeviceMemory. On success Vulkan takes ownership of
        // the fd and closes it when this memory is freed. We hand it over here
        // (forget the OwnedFd) only AFTER allocate_memory succeeds; if it fails
        // the OwnedFd still owns the fd and closes it on drop via `?`.
        let raw_fd = fd_for_import.as_raw_fd();
        let mut import_fd_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(raw_fd);

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index)
            .push_next(&mut import_fd_info);

        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(err) => {
                // allocate_memory failed: fd still owned by OwnedFd (auto-closed
                // on drop). Destroy the image we already created.
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(err);
            }
        };
        // Vulkan now owns the fd — detach it from the OwnedFd so its Drop does
        // NOT close it again (would be a double-close).
        let _ = fd_for_import.into_raw_fd();

        // Bind imported memory to the image.
        if let Err(err) = unsafe { self.device.bind_image_memory(image, memory, 0) } {
            // fd already handed to Vulkan (freed with memory below). Bind failed:
            // destroy the image and free the memory (which also closes the fd).
            unsafe {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
            }
            return Err(err);
        }

        Ok(ImportedFrame {
            image,
            memory,
            width,
            height,
        })
    }

    /// Ensure a VkImage exists for the given PipeWire fd (cache by stable fd, no dup).
    /// Cache hit + matching size → no-op. Miss → dup fd, import (fd ownership goes
    /// to Vulkan on success).
    pub fn ensure_frame_imported(
        &mut self,
        pw_fd: RawFd,
        width: u32,
        height: u32,
        modifier: u64,
        stride: u32,
    ) -> Result<(), vk::Result> {
        let need_create = match self.image_cache.get(&pw_fd) {
            Some(frame) => frame.width != width || frame.height != height,
            None => true,
        };
        if need_create {
            // Evict stale cache entry (size changed).
            if let Some(old) = self.image_cache.remove(&pw_fd) {
                unsafe {
                    self.device.destroy_image(old.image, None);
                    self.device.free_memory(old.memory, None);
                }
            }
            // dup: PipeWire fd is valid for the stream lifetime, but we need our
            // own fd for the import (Vulkan takes ownership of it on success).
            let dup_fd = unsafe { libc::dup(pw_fd) };
            if dup_fd < 0 {
                return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
            }
            // SAFETY: dup() just returned this fd; we own it exclusively.
            let dup_fd = unsafe { OwnedFd::from_raw_fd(dup_fd) };
            let frame = self.import_image_raw(dup_fd, width, height, modifier, stride)?;
            self.image_cache.insert(pw_fd, frame);
        }
        Ok(())
    }

    /// Create CPU-visible linear buffer (private, called from constructor).
    fn create_cpu_buffer_impl(
        device: &Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        width: u32,
        height: u32,
    ) -> Result<(vk::Buffer, vk::DeviceMemory), vk::Result> {
        let size = (width * height * 4) as u64;

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe { device.create_buffer(&buffer_info, None)? };
        let mem_reqs = unsafe { device.get_buffer_memory_requirements(buffer) };

        let mem_type_index = find_memory_type_in(
            memory_properties,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .expect("Host visible memory not found");

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index);

        let memory = unsafe { device.allocate_memory(&alloc_info, None)? };
        unsafe { device.bind_buffer_memory(buffer, memory, 0)? };

        Ok((buffer, memory))
    }

    /// High-level: import frame + crop to CPU buffer + return mapped pixels.
    /// GPU transfer result is immediately visible (HOST_COHERENT, persistent map).
    pub fn capture_crop(
        &mut self,
        pw_fd: RawFd,
        width: u32,
        height: u32,
        modifier: u64,
        stride: u32,
    ) -> Result<&[u8], vk::Result> {
        self.ensure_frame_imported(pw_fd, width, height, modifier, stride)?;
        self.extract_crop_to_cpu(
            pw_fd,
            self.cpu_buffer,
            self.crop_offset_x,
            self.crop_offset_y,
            self.crop_w,
            self.crop_h,
        )?;

        let size = (self.crop_w * self.crop_h * 4) as usize;
        let ptr = self.mapped_ptr;
        // SAFETY: ptr is persistently mapped in new(), unmapped in Drop.
        // HOST_COHERENT ensures GPU writes are visible without manual flush.
        Ok(unsafe { std::slice::from_raw_parts(ptr, size) })
    }

    /// Copy crop from tiled VkImage to linear CPU buffer (HW untile).
    /// Uses FOREIGN_EXT→self ownership transfer (stale frame fix).
    fn extract_crop_to_cpu(
        &self,
        pw_fd: RawFd,
        dst_buffer: vk::Buffer,
        crop_x: i32,
        crop_y: i32,
        crop_w: u32,
        crop_h: u32,
    ) -> Result<(), vk::Result> {
        let frame = match self.image_cache.get(&pw_fd) {
            Some(f) => f,
            None => return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE),
        };
        let cb = self.command_buffer;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe {
            // Pool uses RESET_COMMAND_BUFFER — begin_command_buffer handles reset.
            self.device
                .begin_command_buffer(cb, &begin_info)
                .expect("begin_command_buffer failed");

            // 1. ACQUIRE barrier: claim ownership from FOREIGN_EXT source.
            //    Forces driver to see the compositor's fresh write in the dma-buf.
            let acquire_barrier = vk::ImageMemoryBarrier::builder()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(self.queue_family_index)
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .image(frame.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            self.device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                std::slice::from_ref(&acquire_barrier),
            );

            // 2. Copy target region to linear buffer.
            let region = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D {
                    x: crop_x,
                    y: crop_y,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: crop_w,
                    height: crop_h,
                    depth: 1,
                });

            self.device.cmd_copy_image_to_buffer(
                cb,
                frame.image,
                vk::ImageLayout::GENERAL,
                dst_buffer,
                std::slice::from_ref(&region),
            );

            // 3. RELEASE barrier: return ownership to FOREIGN_EXT so the
            //    compositor can write the next frame. Source access is the
            //    transfer that READ the image (cmd_copy_image_to_buffer).
            let release_barrier = vk::ImageMemoryBarrier::builder()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(self.queue_family_index)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .image(frame.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            self.device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                std::slice::from_ref(&release_barrier),
            );

            self.device
                .end_command_buffer(cb)
                .expect("end_command_buffer failed");

            let submit_info = vk::SubmitInfo::builder().command_buffers(std::slice::from_ref(&cb));

            self.device
                .reset_fences(std::slice::from_ref(&self.fence))
                .expect("reset_fences failed");

            self.device
                .queue_submit(self.queue, std::slice::from_ref(&submit_info), self.fence)
                .expect("queue_submit failed");
            self.device
                .wait_for_fences(std::slice::from_ref(&self.fence), true, u64::MAX)
                .expect("wait_for_fences failed");
        }
        Ok(())
    }
}

/// Free-standing find_memory_type — needed in constructor before `Self` exists.
fn find_memory_type_in(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..memory_properties.memory_type_count).find(|&i| {
        (type_filter & (1 << i)) != 0
            && (memory_properties.memory_types[i as usize].property_flags & properties)
                == properties
    })
}

impl Drop for VulkanDmaBufBackend {
    fn drop(&mut self) {
        unsafe {
            self.device.device_wait_idle().ok();
            // Free all cached VkImages (cache owns them instead of per-frame destroy).
            for frame in self.image_cache.values() {
                self.device.destroy_image(frame.image, None);
                self.device.free_memory(frame.memory, None);
            }
            self.device.destroy_fence(self.fence, None);
            // destroy_command_pool implicitly frees all its command buffers
            self.device.destroy_command_pool(self.command_pool, None);
            // CPU crop buffer — unmap before free.
            self.device.unmap_memory(self.cpu_memory);
            self.device.destroy_buffer(self.cpu_buffer, None);
            self.device.free_memory(self.cpu_memory, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
