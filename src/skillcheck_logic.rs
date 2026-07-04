//! Skill check detection and click logic.
//! Pure pixel analysis — no Vulkan, no PipeWire.

use crate::config::Config;
use crate::input::{InputEmulatorBackend, KeyboardEmulator};
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
            dark_value_threshold: c.dark_val as f32,
            inner_enter: d.inner_enter as f32,
            inner_exit: d.inner_exit as f32,
            ring_boost: d.ring_threshold as f32,
            ring_discount: d.ring_discount as f32,
            grey_v_min: c.grey_val_min as f32,
            grey_v_max: c.grey_val_max as f32,
            grey_s_max: c.grey_sat_max as f32,
            red_hue_min: c.red_hue_min as f32,
            red_hue_max: c.red_hue_max as f32,
            red_sat_min: c.red_sat_min as f32,
            red_val_min: c.red_val_min as f32,
            white_sat_max: c.white_sat_max as f32,
            white_val_min: c.white_val_min as f32,
            speed_history_min: t.speed_history_min,
            latency_ms: t.latency_ms as f32,
            calibrating_samples: t.calibrating_samples,
            active_miss: t.active_miss,
            calibrating_miss: t.calibrating_miss,
        }
    }
}

pub struct WiggleContext {
    pub last_angle: f32,
    pub unwrapped_angle: f32,
    pub history: VecDeque<(Instant, f32)>,
    pub angular_speed: f32,
    pub consecutive_misses: u32,
    pub last_click_time: Option<Instant>,
}

pub struct BasicContext {
    pub target_angle: f32,
    pub last_angle: f32,
    pub unwrapped_angle: f32,
    pub history: VecDeque<(Instant, f32)>,
    pub angular_speed: f32,
    pub has_clicked: bool,
    pub consecutive_misses: u32,
    pub click_time: Option<Instant>,
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
    Basic(BasicContext),
    Wiggle(WiggleContext),
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

/// Scan 360° of the circle, return (white_zone_center, red_pointer_angle, white_edges, is_white).
fn scan_angles(
    image: &[u8],
    stride: usize,
    circle_pattern: &[Pixel],
    params: &SkillCheckParams,
    _log_tx: &tokio::sync::mpsc::Sender<String>,
) -> (Option<f32>, Option<f32>, Option<(f32, f32)>, [bool; 360]) {
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
    (target_angle, pointer_angle, edges, is_white)
}

/// Check if the white pattern is a wiggle.
fn is_wiggle(is_white: &[bool; 360]) -> bool {
    let mut left_count = 0;
    let mut right_count = 0;
    // Left zone (180 deg)
    for pixel in 160..200 {
        if is_white[pixel] {
            left_count += 1;
        }
    }
    // Right zone (0/360 deg)
    for pixel in 0..20 {
        if is_white[pixel] {
            right_count += 1;
        }
    }
    for pixel in 340..360 {
        if is_white[pixel] {
            right_count += 1;
        }
    }
    left_count > 5 && right_count > 5
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
    log_tx: &tokio::sync::mpsc::Sender<String>,
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
        SkillCheckState::Basic(_)
            | SkillCheckState::Wiggle(_)
            | SkillCheckState::Calibrating { .. }
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

    if !widget_visible && ring_ok && inner_ratio > 0.35 {
        let (target_angle, pointer_angle, edges, is_white) =
            scan_angles(pixels, stride, circle_pattern, params, log_tx);
        if target_angle.is_some() && pointer_angle.is_some() && edges.is_some() {
            widget_visible = true;
            pre_scanned = Some((target_angle, pointer_angle, edges, is_white));
        }
    }

    if !widget_visible {
        if let SkillCheckState::Basic(ctx) = state {
            ctx.consecutive_misses += 1;
            if ctx.consecutive_misses as usize >= params.active_miss {
                log_tx
                    .try_send(format!(
                        "Skillcheck inactive ({} misses).",
                        ctx.consecutive_misses
                    ))
                    .ok();
                *state = SkillCheckState::InSearch;
            }
        } else if let SkillCheckState::Wiggle(ctx) = state {
            ctx.consecutive_misses += 1;
            if ctx.consecutive_misses as usize >= params.active_miss {
                log_tx
                    .try_send(format!(
                        "Wiggle inactive ({} misses).",
                        ctx.consecutive_misses
                    ))
                    .ok();
                *state = SkillCheckState::InSearch;
            }
        } else if let SkillCheckState::Calibrating { misses, .. } = state {
            *misses += 1;
            if *misses as usize >= params.calibrating_miss {
                log_tx
                    .try_send(format!(
                        "Skillcheck lost during calibration ({} misses).",
                        misses
                    ))
                    .ok();
                *state = SkillCheckState::InSearch;
            }
        }
        return;
    }
    if let SkillCheckState::Basic(ctx) = state {
        ctx.consecutive_misses = 0;
    } else if let SkillCheckState::Wiggle(ctx) = state {
        ctx.consecutive_misses = 0;
    }

    match state {
        SkillCheckState::InSearch => {
            let (target_angle, pointer_angle, edges, is_white) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params, log_tx),
            };
            if let (Some(target), Some(pointer)) = (target_angle, pointer_angle) {
                // Wiggle detected! Target (approx): 180 or 0 deg.
                let is_wiggle = is_wiggle(&is_white);

                if is_wiggle {
                    log_tx
                        .try_send(format!(
                            "Wiggle detected! Target (approx): {:.1}°, Pointer: {:.1}°",
                            target, pointer
                        ))
                        .ok();
                    let mut history = VecDeque::with_capacity(params.speed_history_min.max(8));
                    history.push_back((Instant::now(), pointer));
                    *state = SkillCheckState::Wiggle(WiggleContext {
                        last_angle: pointer,
                        unwrapped_angle: pointer,
                        history,
                        angular_speed: 0.0,
                        consecutive_misses: 0,
                        last_click_time: None,
                    });
                    return;
                }

                let is_at_start = pointer < 25.0 || pointer > 345.0;
                if !is_at_start {
                    return;
                }
                if let Some((s, e)) = edges {
                    log_tx
                        .try_send(format!(
                            "Skillcheck detected! Target: {:.1}°, Pointer: {:.1}° (Edges: {:.0}-{:.0}°)",
                            target, pointer, s, e
                        ))
                        .ok();
                }
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
            misses,
        } => {
            let (target_angle, pointer_angle, _edges, _) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params, log_tx),
            };

            if let Some(pointer) = pointer_angle {
                let mut samples = target_samples.clone();
                if let Some(t) = target_angle {
                    samples.push(t);
                }
                if samples.len() >= params.calibrating_samples {
                    let avg = samples.iter().sum::<f32>() / samples.len() as f32;
                    log_tx
                        .try_send(format!(
                            "Target calibrated: {:.1}° ({} samples)",
                            avg,
                            samples.len()
                        ))
                        .ok();
                    let mut history = VecDeque::with_capacity(params.speed_history_min.max(8));
                    history.push_back((Instant::now(), pointer));
                    *state = SkillCheckState::Basic(BasicContext {
                        target_angle: avg,
                        last_angle: pointer,
                        unwrapped_angle: pointer,
                        history,
                        angular_speed: 0.0,
                        has_clicked: false,
                        consecutive_misses: 0,
                        click_time: None,
                    });
                } else {
                    *state = SkillCheckState::Calibrating {
                        target_samples: samples,
                        pointer,
                        misses: 0,
                    };
                }
            } else {
                let new_misses = *misses + 1;
                if new_misses as usize >= params.calibrating_miss {
                    let _ = log_tx.try_send(format!(
                        "Calibration aborted: inner={:.2}/{:.2}, ring={:.2}/{:.2}, target={}, pointer={}",
                        inner_ratio,
                        inner_thr,
                        ring_ratio,
                        params.ring_boost,
                        target_angle.is_some(),
                        pointer_angle.is_some()
                    ));
                    *state = SkillCheckState::InSearch;
                } else {
                    *state = SkillCheckState::Calibrating {
                        target_samples: target_samples.clone(),
                        pointer: *init_pointer,
                        misses: new_misses,
                    };
                }
            }
        }
        SkillCheckState::Basic(ctx) => {
            if ctx.has_clicked {
                if let Some(click_t) = ctx.click_time {
                    if click_t.elapsed().as_millis() >= 200 {
                        *state = SkillCheckState::InSearch;
                    }
                }
                return;
            }
            let (_, pointer_angle, _edges, _) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params, log_tx),
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
            if ctx.history.len() >= params.speed_history_min {
                if let Some(speed) = compute_speed_least_squares(&ctx.history) {
                    if speed > 0.0 && speed < 2.0 {
                        ctx.angular_speed = speed;
                    }
                }
            }

            // Click decision.
            if ctx.angular_speed > 0.0 {
                let unwrapped_target = unwrap_target(ctx.target_angle, ctx.unwrapped_angle);
                let angle_to_go = unwrapped_target - ctx.unwrapped_angle;
                if angle_to_go > 0.0 {
                    let time_to_go = angle_to_go / ctx.angular_speed;
                    if time_to_go <= params.latency_ms {
                        log_tx
                            .try_send(format!(
                                "CLICK Target: {:.1}, Pointer: {:.1}, Speed: {:.4} deg/ms",
                                ctx.target_angle, pointer, ctx.angular_speed
                            ))
                            .ok();
                        if let Err(_e) = input_emulator.press_space() {}
                        ctx.has_clicked = true;
                        ctx.click_time = Some(Instant::now());
                    }
                }
            }
        }
        SkillCheckState::Wiggle(ctx) => {
            let (_, pointer_angle, _edges, _) = match pre_scanned {
                Some(angles) => angles,
                None => scan_angles(pixels, stride, circle_pattern, params, log_tx),
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
            if diff.abs() < 0.001 {
                return;
            }

            ctx.unwrapped_angle += diff.abs();
            ctx.last_angle = pointer;
            ctx.history.push_back((now, ctx.unwrapped_angle));
            if ctx.history.len() > 15 {
                ctx.history.pop_front();
            }

            // Wiggle speed is very linear; 3 frames (~50ms) is enough to estimate it.
            if ctx.history.len() >= 3 {
                if let Some(speed) = compute_speed_least_squares(&ctx.history) {
                    if speed > 0.0 && speed < 3.0 {
                        ctx.angular_speed = speed;
                    }
                }
            }

            // Dynamic target based on arrow direction:
            // If moving clockwise (diff > 0), target is 0.0 (Right).
            // If moving counter-clockwise (diff < 0), target is 180.0 (Left).
            let target_angle = if diff > 0.0 { 0.0 } else { 180.0 };

            let mut angle_to_go = target_angle - pointer;
            if angle_to_go < -180.0 {
                angle_to_go += 360.0;
            } else if angle_to_go > 180.0 {
                angle_to_go -= 360.0;
            }

            // Since target is always in front of our movement direction, correct_direction is always true.
            // But we keep the check to prevent glitches at direction transition.
            let correct_direction =
                (diff > 0.0 && angle_to_go > 0.0) || (diff < 0.0 && angle_to_go < 0.0);

            let mut can_click = true;
            if let Some(last_click) = ctx.last_click_time {
                if last_click.elapsed().as_millis() < 250 {
                    can_click = false;
                }
            }

            if correct_direction && ctx.angular_speed > 0.0 && can_click {
                let dist = angle_to_go.abs();
                let time_to_go = dist / ctx.angular_speed;
                if time_to_go <= params.latency_ms {
                    log_tx
                        .try_send(format!(
                            "WIGGLE CLICK Target: {} ({:.1}°), Pointer: {:.1}°, Speed: {:.4} deg/ms",
                            if target_angle == 180.0 { "Left" } else { "Right" },
                            target_angle,
                            pointer,
                            ctx.angular_speed
                        ))
                        .ok();
                    if let Err(_e) = input_emulator.press_space() {}
                    ctx.last_click_time = Some(Instant::now());
                }
            }
        }
    }
}
