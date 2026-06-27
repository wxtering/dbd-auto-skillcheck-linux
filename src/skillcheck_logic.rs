//! Skill check detection and click logic.
//! Pure pixel analysis — no Vulkan, no PipeWire.

use crate::config::Config;
use crate::input::KeyboardEmulator;
use std::collections::VecDeque;
use std::time::Instant;

// pixel position on the screen
pub struct Pixel {
    pub x: u32,
    pub y: u32,
}

pub struct Circle {
    pub center: Pixel,
    pub radius: u32,
    #[allow(dead_code)]
    pub diameter: u32,
}

/// All tunable parameters for skillcheck processing (built from Config).
/// Passed into process_skillcheck_frame every frame.
#[derive(Clone)]
pub struct SkillCheckParams {
    // detection
    pub dark_value_threshold: f32,
    pub inner_enter: f32,
    pub inner_exit: f32,
    pub ring_boost: f32,
    pub ring_discount: f32,
    pub grey_v_min: f32,
    pub grey_v_max: f32,
    pub grey_s_max: f32,
    // colors
    pub red_hue_min: f32,
    pub red_hue_max: f32,
    pub red_sat_min: f32,
    pub red_val_min: f32,
    pub white_sat_max: f32,
    pub white_val_min: f32,
    // timing
    pub speed_history_min: usize,
    pub latency_ms: f32,
    pub calibrating_samples: usize,
    pub active_miss: usize,
    pub calibrating_miss: usize,
}

impl From<&Config> for SkillCheckParams {
    fn from(cfg: &Config) -> Self {
        let d = &cfg.detection;
        let c = &cfg.colors;
        let t = &cfg.timing;
        Self {
            dark_value_threshold: d.dark_value_threshold,
            inner_enter: d.inner_enter,
            inner_exit: d.inner_exit,
            ring_boost: d.ring_boost,
            ring_discount: d.ring_discount,
            grey_v_min: d.grey_v_min,
            grey_v_max: d.grey_v_max,
            grey_s_max: d.grey_s_max,
            red_hue_min: c.red_hue_min,
            red_hue_max: c.red_hue_max,
            red_sat_min: c.red_sat_min,
            red_val_min: c.red_val_min,
            white_sat_max: c.white_sat_max,
            white_val_min: c.white_val_min,
            speed_history_min: t.speed_history_min,
            latency_ms: t.latency_ms,
            calibrating_samples: t.calibrating_samples,
            active_miss: t.active_miss,
            calibrating_miss: t.calibrating_miss,
        }
    }
}

pub struct ActiveContext {
    pub target_angle: f32,
    pub last_angle: f32,
    pub unwrapped_angle: f32,
    pub history: VecDeque<(Instant, f32)>,
    pub angular_speed: f32,
    pub has_clicked: bool,
    pub consecutive_misses: u32,
}

#[derive(Default)]
pub enum SkillCheckState {
    #[default]
    InSearch,
    /// Skillcheck found, target being averaged over N frames (noise reduction).
    Calibrating {
        target_samples: Vec<f32>,
        pointer: f32,
        misses: u32,
    },
    Active(ActiveContext),
}

pub fn generate_patterns(circle: &Circle) -> (Vec<Pixel>, Vec<Pixel>, Vec<Pixel>) {
    let cx = circle.center.x as i32;
    let cy = circle.center.y as i32;
    let r_circle = circle.radius as f32;

    let mut circle_pattern = Vec::with_capacity(360 * 5);
    for angle in 0..360 {
        let rad = (angle as f32).to_radians();
        for dr in -2..=2 {
            let r = r_circle + dr as f32;
            let x = cx + (r * rad.cos()) as i32;
            let y = cy + (r * rad.sin()) as i32;
            circle_pattern.push(Pixel {
                x: x as u32,
                y: y as u32,
            });
        }
    }

    let half_width = (r_circle * 0.46) as i32;
    let dist_near = (r_circle * 0.30) as i32;
    let dist_far = (r_circle * 0.70) as i32;

    let mut inner_pattern = Vec::new();
    let step_x = (half_width / 5).max(1);
    let step_y = ((dist_far - dist_near) / 6).max(1);

    for y in (cy - dist_far..=cy - dist_near).step_by(step_y as usize) {
        for x in (cx - half_width..=cx + half_width).step_by(step_x as usize) {
            inner_pattern.push(Pixel {
                x: x as u32,
                y: y as u32,
            });
        }
    }
    for y in (cy + dist_near..=cy + dist_far).step_by(step_y as usize) {
        for x in (cx - half_width..=cx + half_width).step_by(step_x as usize) {
            inner_pattern.push(Pixel {
                x: x as u32,
                y: y as u32,
            });
        }
    }

    let pointer_x_min = cx - (r_circle * 0.11) as i32;
    let pointer_x_max = cx + (r_circle * 0.11) as i32;
    let pointer_y_min = cy - r_circle as i32 - (r_circle * 0.23) as i32;
    let pointer_y_max = cy - r_circle as i32 + (r_circle * 0.11) as i32;
    let mut pointer_pattern = Vec::new();
    for y in pointer_y_min..=pointer_y_max {
        for x in pointer_x_min..=pointer_x_max {
            pointer_pattern.push(Pixel {
                x: x as u32,
                y: y as u32,
            });
        }
    }
    (circle_pattern, inner_pattern, pointer_pattern)
}

/// Count dark pixels inside the progress fill area.
fn count_dark_inner(image: &[u8], stride: usize, inner: &[Pixel], threshold: f32) -> usize {
    let mut m = 0;
    for p in inner {
        let idx = p.y as usize * stride + p.x as usize * 4;
        if idx + 3 < image.len() {
            let (_, _, v) = bgr_to_hsv(image[idx], image[idx + 1], image[idx + 2]);
            if v < threshold {
                m += 1;
            }
        }
    }
    m
}

/// Count grey ring pixels (opaque ring, background-independent).
fn count_grey_ring(
    image: &[u8],
    stride: usize,
    circle: &[Pixel],
    vmin: f32,
    vmax: f32,
    smax: f32,
) -> usize {
    let mut grey = 0;
    for p in circle {
        let idx = p.y as usize * stride + p.x as usize * 4;
        if idx + 3 < image.len() {
            let (_, s, v) = bgr_to_hsv(image[idx], image[idx + 1], image[idx + 2]);
            if v > vmin && v < vmax && s < smax {
                grey += 1;
            }
        }
    }
    grey
}

fn find_white_edges(angles_mask: &[bool; 360]) -> Option<(f32, f32)> {
    let mut best_start = 0;
    let mut best_len = 0u32;
    let mut cur_start = 0;
    let mut cur_len = 0u32;
    for i in 0..720 {
        let idx = i % 360;
        if angles_mask[idx] {
            if cur_len == 0 {
                cur_start = idx;
            }
            cur_len += 1;
        } else {
            if cur_len > best_len {
                best_len = cur_len;
                best_start = cur_start;
            }
            cur_len = 0;
        }
    }
    if cur_len > best_len {
        best_len = cur_len;
        best_start = cur_start;
    }
    if best_len >= 5 {
        let start = best_start as f32;
        let end = ((best_start + best_len as usize) % 360) as f32;
        Some((start, if end <= start { end + 360.0 } else { end }))
    } else {
        None
    }
}

/// Circular mean of a binary mask over 360°.
fn find_cluster_center(angles_mask: &[bool; 360]) -> Option<f32> {
    let mut sum_cos = 0.0;
    let mut sum_sin = 0.0;
    let mut count = 0;
    for (angle, &matched) in angles_mask.iter().enumerate() {
        if matched {
            let rad = (angle as f32).to_radians();
            sum_cos += rad.cos();
            sum_sin += rad.sin();
            count += 1;
        }
    }
    if count < 3 {
        return None;
    }
    let mut mean_rad = sum_sin.atan2(sum_cos);
    if mean_rad < 0.0 {
        mean_rad += 2.0 * std::f32::consts::PI;
    }
    Some(mean_rad.to_degrees())
}

/// Scan 360° of the circle, return (white_zone_center, red_pointer_angle, white_edges).
fn scan_angles(
    image: &[u8],
    stride: usize,
    circle_pattern: &[Pixel],
    params: &SkillCheckParams,
) -> (Option<f32>, Option<f32>, Option<(f32, f32)>) {
    let mut is_red = [false; 360];
    let mut is_white = [false; 360];
    for angle in 0..360 {
        for dr in 0..5 {
            let p = &circle_pattern[angle * 5 + dr];
            let idx = p.y as usize * stride + p.x as usize * 4;
            if idx + 3 < image.len() {
                let (h, s, v) = bgr_to_hsv(image[idx], image[idx + 1], image[idx + 2]);
                if is_red_color(h, s, v, params) {
                    is_red[angle] = true;
                }
                if is_white_color(h, s, v, params) {
                    is_white[angle] = true;
                }
            }
        }
    }
    let edges = find_white_edges(&is_white);
    let target_angle = edges.map(|(s, e)| (s + e) / 2.0);
    let pointer_angle = find_cluster_center(&is_red);
    (target_angle, pointer_angle, edges)
}

/// Unwrap angle to be monotonically ≥ current.
fn unwrap_target(target: f32, current: f32) -> f32 {
    let mut u = target;
    while u < current {
        u += 360.0;
    }
    u
}

fn bgr_to_hsv(b: u8, g: u8, r: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let min = r.min(g.min(b));
    let max = r.max(g.max(b));
    let delta = max - min;
    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        let mut h = 60.0 * (((g - b) / delta) % 6.0);
        if h < 0.0 {
            h += 360.0;
        }
        h
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    (h, s, v)
}

fn is_red_color(h: f32, s: f32, v: f32, p: &SkillCheckParams) -> bool {
    !(p.red_hue_min..=p.red_hue_max).contains(&h) && s > p.red_sat_min && v > p.red_val_min
}

fn is_white_color(_h: f32, s: f32, v: f32, p: &SkillCheckParams) -> bool {
    s < p.white_sat_max && v > p.white_val_min
}

/// Least-squares slope of (Instant, unwrapped_angle) history → deg/ms.
fn compute_speed_least_squares(history: &VecDeque<(Instant, f32)>) -> Option<f32> {
    if history.len() < 2 {
        return None;
    }
    let (t0, _) = history[0];
    let n = history.len() as f32;
    let mut sum_t = 0.0;
    let mut sum_a = 0.0;
    let mut sum_tt = 0.0;
    let mut sum_ta = 0.0;
    for (t, a) in history {
        let tms = t.duration_since(t0).as_secs_f32() * 1000.0;
        sum_t += tms;
        sum_a += a;
        sum_tt += tms * tms;
        sum_ta += tms * a;
    }
    let denom = n * sum_tt - sum_t * sum_t;
    if denom <= 0.0 {
        return None;
    }
    Some((n * sum_ta - sum_t * sum_a) / denom)
}

/// Main entry: process one frame, decide whether to click.
pub fn process_skillcheck_frame(
    pixels: &[u8],
    stride: usize,
    patternes: &(Vec<Pixel>, Vec<Pixel>, Vec<Pixel>),
    state: &mut SkillCheckState,
    params: &SkillCheckParams,
    input_emulator: &mut KeyboardEmulator,
) {
    let (circle_pattern, inner_pattern, _pointer_pattern) = patternes;

    let inner_ratio = count_dark_inner(pixels, stride, inner_pattern, params.dark_value_threshold)
        as f32
        / inner_pattern.len() as f32;
    let ring_ratio = count_grey_ring(
        pixels,
        stride,
        circle_pattern,
        params.grey_v_min,
        params.grey_v_max,
        params.grey_s_max,
    ) as f32
        / circle_pattern.len() as f32;

    // Ring is a discount on inner threshold (NOT a standalone detector).
    let currently_active = matches!(
        state,
        SkillCheckState::Active(_) | SkillCheckState::Calibrating { .. }
    );
    let ring_ok = ring_ratio >= params.ring_boost;
    let (inner_thr_base, disc) = if currently_active {
        (params.inner_exit, params.ring_discount)
    } else {
        (params.inner_enter, params.ring_discount)
    };
    let inner_thr = if ring_ok {
        inner_thr_base - disc
    } else {
        inner_thr_base
    };
    let mut widget_visible = inner_ratio >= inner_thr;
    let mut pre_scanned = None;

    if !widget_visible && ring_ok {
        let (target_angle, pointer_angle, edges) =
            scan_angles(pixels, stride, circle_pattern, params);
        if target_angle.is_some() && pointer_angle.is_some() && edges.is_some() {
            widget_visible = true;
            pre_scanned = Some((target_angle, pointer_angle, edges));
        }
    }

    if !widget_visible {
        if let SkillCheckState::Active(ctx) = state {
            ctx.consecutive_misses += 1;
            if ctx.consecutive_misses as usize >= params.active_miss {
                println!("Skillcheck inactive ({} misses).", params.active_miss);
                *state = SkillCheckState::InSearch;
            }
        } else if let SkillCheckState::Calibrating { misses, .. } = state {
            *misses += 1;
            if *misses as usize >= params.calibrating_miss {
                println!(
                    "Skillcheck lost during calibration ({} misses).",
                    params.calibrating_miss
                );
                *state = SkillCheckState::InSearch;
            }
        }
        return;
    }
    if let SkillCheckState::Active(ctx) = state {
        ctx.consecutive_misses = 0;
    }

    match state {
        SkillCheckState::InSearch => {
            let (target_angle, pointer_angle, edges) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params),
            };
            if let (Some(target), Some(pointer)) = (target_angle, pointer_angle) {
                // Стрелка в DbD всегда стартует на 12 часах (0/360 градусов).
                // Учитываем небольшой сдвиг за первые кадры (до 25°).
                let is_at_start = pointer < 25.0 || pointer > 345.0;
                if !is_at_start {
                    return;
                }
                if let Some((start, end)) = edges {
                    println!(
                        "White zone edges: {:.0}-{:.0}° (width {:.0}°)",
                        start,
                        end,
                        if end > start {
                            end - start
                        } else {
                            end + 360.0 - start
                        }
                    );
                }
                println!(
                    "Skillcheck detected! Target: {:.1}°, Pointer: {:.1}°",
                    target, pointer
                );
                *state = SkillCheckState::Calibrating {
                    target_samples: vec![target],
                    pointer,
                    misses: 0,
                };
            }
        }
        SkillCheckState::Calibrating {
            target_samples,
            pointer: init_pointer,
            ..
        } => {
            let (target_angle, pointer_angle, _edges) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params),
            };
            let pointer = pointer_angle.unwrap_or(*init_pointer);
            let mut samples = target_samples.clone();
            if let Some(t) = target_angle {
                samples.push(t);
            }
            if samples.len() >= params.calibrating_samples {
                let avg = samples.iter().sum::<f32>() / samples.len() as f32;
                println!("Target calibrated: {:.1}° ({} samples)", avg, samples.len());
                let mut history = VecDeque::with_capacity(params.speed_history_min.max(8));
                history.push_back((Instant::now(), pointer));
                *state = SkillCheckState::Active(ActiveContext {
                    target_angle: avg,
                    last_angle: pointer,
                    unwrapped_angle: pointer,
                    history,
                    angular_speed: 0.0,
                    has_clicked: false,
                    consecutive_misses: 0,
                });
            } else {
                *state = SkillCheckState::Calibrating {
                    target_samples: samples,
                    pointer,
                    misses: 0,
                };
            }
        }
        SkillCheckState::Active(ctx) => {
            if ctx.has_clicked {
                return;
            }
            let (_, pointer_angle, _edges) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params),
            };
            let Some(pointer) = pointer_angle else {
                return;
            };

            let mut diff = pointer - ctx.last_angle;
            if diff < -180.0 {
                diff += 360.0;
            } else if diff > 180.0 {
                diff -= 360.0;
            }

            let now = Instant::now();
            if diff <= 0.0 {
                ctx.last_angle = pointer;
                return;
            }

            ctx.unwrapped_angle += diff;
            ctx.last_angle = pointer;
            ctx.history.push_back((now, ctx.unwrapped_angle));

            // Recompute speed every frame over ALL accumulated samples.
            if ctx.history.len() >= params.speed_history_min
                && let Some(speed) = compute_speed_least_squares(&ctx.history)
                && speed > 0.0
                && speed < 2.0
            {
                ctx.angular_speed = speed;
            }

            // Click decision.
            if ctx.angular_speed > 0.0 {
                let unwrapped_target = unwrap_target(ctx.target_angle, ctx.unwrapped_angle);
                let angle_to_go = unwrapped_target - ctx.unwrapped_angle;
                if angle_to_go > 0.0 {
                    let time_to_go = angle_to_go / ctx.angular_speed;
                    if time_to_go <= params.latency_ms {
                        println!(
                            "___CLICK__ Target: {:.1}, Pointer: {:.1}, Speed: {:.4} deg/ms (Time to go: {:.1} ms)",
                            unwrapped_target, ctx.unwrapped_angle, ctx.angular_speed, time_to_go
                        );
                        if let Err(e) = input_emulator.press_space() {
                            eprintln!("Failed to simulate spacebar press: {:?}", e);
                        }
                        ctx.has_clicked = true;
                    }
                }
            }
        }
    }
}
