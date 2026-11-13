//! The touchscreen, read straight from its evdev device.
//!
//! The Nodo's panel has a Goodix controller. Only single-finger presses, drags and releases matter,
//! so this reads the first touch's position and the touch button and nothing else.

use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

/// `struct input_event` on 64-bit Linux: a 16-byte timeval, then type, code and value.
const EVENT_SIZE: usize = 24;

/// A finger on the glass, each axis scaled to 0..=1 in the device's own orientation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Touch {
    Down(f64, f64),
    Move(f64, f64),
    Up,
}

/// Turns raw events into [`Touch`]es, one per completed report.
#[derive(Debug, Clone)]
pub struct Decoder {
    max: [i32; 2],
    pos: [i32; 2],
    down: bool,
    was_down: bool,
    moved: bool,
}

impl Decoder {
    pub fn new(max_x: i32, max_y: i32) -> Self {
        Decoder { max: [max_x.max(1), max_y.max(1)], pos: [0, 0], down: false, was_down: false, moved: false }
    }

    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<Touch> {
        match (kind, code) {
            (EV_ABS, ABS_MT_POSITION_X | ABS_X) => {
                self.pos[0] = value;
                self.moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y | ABS_Y) => {
                self.pos[1] = value;
                self.moved = true;
            }
            (EV_KEY, BTN_TOUCH) => self.down = value != 0,
            (EV_SYN, SYN_REPORT) => {
                let x = f64::from(self.pos[0].clamp(0, self.max[0])) / f64::from(self.max[0]);
                let y = f64::from(self.pos[1].clamp(0, self.max[1])) / f64::from(self.max[1]);
                let event = match (self.was_down, self.down) {
                    (false, true) => Some(Touch::Down(x, y)),
                    (true, true) if self.moved => Some(Touch::Move(x, y)),
                    (true, false) => Some(Touch::Up),
                    _ => None,
                };
                self.was_down = self.down;
                self.moved = false;
                return event;
            }
            _ => {}
        }
        None
    }
}

/// The event device whose name starts with `Goodix`, under `sys_root` (`/` outside tests).
pub fn find_device(sys_root: &Path) -> Option<PathBuf> {
    let class = sys_root.join("sys/class/input");
    let mut names: Vec<_> = std::fs::read_dir(class).ok()?.flatten().map(|e| e.file_name()).collect();
    names.sort();
    names.into_iter().find_map(|name| {
        let name = name.to_string_lossy().into_owned();
        if !name.starts_with("event") {
            return None;
        }
        let label = std::fs::read_to_string(sys_root.join("sys/class/input").join(&name).join("device/name")).ok()?;
        label.trim().starts_with("Goodix").then(|| PathBuf::from("/dev/input").join(name))
    })
}

/// `EVIOCGABS(code)`: an axis's range, so positions can be scaled.
fn axis_max(file: &File, code: u16) -> Option<i32> {
    let mut info = [0i32; 6]; // value, minimum, maximum, fuzz, flat, resolution
    let size = std::mem::size_of_val(&info) as libc::c_ulong;
    let request = (2 << 30) | (size << 16) | ((b'E' as libc::c_ulong) << 8) | (0x40 + libc::c_ulong::from(code));
    // SAFETY: the ioctl writes one `struct input_absinfo`, six ints, into `info`.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request as _, info.as_mut_ptr()) };
    (result == 0).then_some(info[2])
}

/// Opens the device and calls `on_touch` for every touch until reading fails.
pub fn run(device: &Path, mut on_touch: impl FnMut(Touch) -> bool) -> std::io::Result<()> {
    let mut file = File::open(device)?;
    let max_x = axis_max(&file, ABS_MT_POSITION_X).or_else(|| axis_max(&file, ABS_X)).unwrap_or(4095);
    let max_y = axis_max(&file, ABS_MT_POSITION_Y).or_else(|| axis_max(&file, ABS_Y)).unwrap_or(4095);
    let mut decoder = Decoder::new(max_x, max_y);
    let mut buf = [0u8; EVENT_SIZE * 32];
    loop {
        let n = file.read(&mut buf)?;
        for event in buf[..n - n % EVENT_SIZE].chunks_exact(EVENT_SIZE) {
            let kind = u16::from_ne_bytes([event[16], event[17]]);
            let code = u16::from_ne_bytes([event[18], event[19]]);
            let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
            if let Some(touch) = decoder.feed(kind, code, value) {
                if !on_touch(touch) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_press_a_drag_and_a_release() {
        let mut d = Decoder::new(1000, 2000);
        let mut out = Vec::new();
        for (kind, code, value) in [
            (EV_ABS, ABS_MT_POSITION_X, 500),
            (EV_ABS, ABS_MT_POSITION_Y, 1000),
            (EV_KEY, BTN_TOUCH, 1),
            (EV_SYN, SYN_REPORT, 0),
            (EV_ABS, ABS_MT_POSITION_X, 250),
            (EV_SYN, SYN_REPORT, 0),
            (EV_SYN, SYN_REPORT, 0), // a report without movement says nothing
            (EV_KEY, BTN_TOUCH, 0),
            (EV_SYN, SYN_REPORT, 0),
        ] {
            out.extend(d.feed(kind, code, value));
        }
        assert_eq!(out, vec![Touch::Down(0.5, 0.5), Touch::Move(0.25, 0.5), Touch::Up]);
    }

    #[test]
    fn finds_the_goodix_device() {
        let root = std::env::temp_dir().join(format!("nodo-touch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (event, name) in [("event0", "gpio-keys"), ("event4", "Goodix Capacitive TouchScreen")] {
            let dir = root.join("sys/class/input").join(event).join("device");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
        }
        assert_eq!(find_device(&root), Some(PathBuf::from("/dev/input/event4")));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(find_device(&root), None);
    }
}
