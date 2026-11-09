//! The parts of nodo-gpu-check that need no GPU: choosing memory, reading versions and checking
//! the compute result.

/// The compute shader: `c[i] = a[i] + b[i]`, 64 invocations per workgroup. `shaders/add.comp`
/// is its source.
pub const ADD_SPV: &[u8] = include_bytes!("../shaders/add.spv");
pub const WORKGROUP: u32 = 64;

/// SPIR-V as the words Vulkan takes. `None` when it is not SPIR-V.
pub fn spirv_words(bytes: &[u8]) -> Option<Vec<u32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (words.first() == Some(&0x0723_0203)).then_some(words)
}

/// `major.minor.patch` of a Vulkan API version.
pub fn api_version(v: u32) -> String {
    format!("{}.{}.{}", (v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
}

/// The first memory type allowed by `type_bits` that has every flag in `wanted`.
pub fn memory_type(type_bits: u32, types: &[u32], wanted: u32) -> Option<u32> {
    (0..types.len().min(32) as u32)
        .find(|&i| type_bits & (1 << i) != 0 && types[i as usize] & wanted == wanted)
}

/// The inputs: `a[i] = i`, `b[i] = 3 * i + 7`, wrapping.
pub fn inputs(n: u32) -> (Vec<u32>, Vec<u32>) {
    let a = (0..n).collect();
    let b = (0..n).map(|i| i.wrapping_mul(3).wrapping_add(7)).collect();
    (a, b)
}

/// The first index where `c` is not `a + b`, if any.
pub fn first_wrong(a: &[u32], b: &[u32], c: &[u32]) -> Option<usize> {
    if a.len() != c.len() || b.len() != c.len() {
        return Some(0);
    }
    (0..c.len()).find(|&i| c[i] != a[i].wrapping_add(b[i]))
}

/// The device limits the Monokulo server's wallet scanner (monokulo-8ar, `src/gpu.rs`) checks
/// before it uses a GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_storage_buffer_range: u32,
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size_x: u32,
    pub max_push_constants_size: u32,
    pub max_per_stage_descriptor_storage_buffers: u32,
}

/// 32,768 outputs of 1,200 bytes in one storage buffer.
pub const SCAN_STORAGE_BUFFER_RANGE: u32 = 32_768 * 1_200;

/// One line per requirement: its name, what the device has, what the scanner needs, and whether
/// the device meets it.
pub fn scanner_limits(l: &Limits) -> Vec<(&'static str, u32, u32, bool)> {
    [
        ("maxStorageBufferRange", l.max_storage_buffer_range, SCAN_STORAGE_BUFFER_RANGE),
        ("maxComputeWorkGroupInvocations", l.max_compute_work_group_invocations, 64),
        ("maxComputeWorkGroupSize[0]", l.max_compute_work_group_size_x, 64),
        ("maxPushConstantsSize", l.max_push_constants_size, 20),
        ("maxPerStageDescriptorStorageBuffers", l.max_per_stage_descriptor_storage_buffers, 4),
    ]
    .into_iter()
    .map(|(name, has, needs)| (name, has, needs, has >= needs))
    .collect()
}

/// Memory property flags, as Vulkan numbers them.
pub const DEVICE_LOCAL: u32 = 0x1;
pub const HOST_VISIBLE: u32 = 0x2;
pub const HOST_COHERENT: u32 = 0x4;

/// The scanner's memory: device-local buffers and host-visible coherent ones.
pub fn scanner_memory(types: &[u32]) -> (bool, bool) {
    let has = |wanted: u32| types.iter().any(|t| t & wanted == wanted);
    (has(DEVICE_LOCAL), has(HOST_VISIBLE | HOST_COHERENT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_limits_compare_against_its_thresholds() {
        // What panvk reports on a Mali-G610 (panvk_vX_physical_device.c), with panthor's
        // 1024 threads per workgroup.
        let panvk = Limits {
            max_storage_buffer_range: u32::MAX,
            max_compute_work_group_invocations: 1024,
            max_compute_work_group_size_x: 1024,
            max_push_constants_size: 256,
            max_per_stage_descriptor_storage_buffers: 64,
        };
        assert!(scanner_limits(&panvk).iter().all(|l| l.3));
        let small = Limits { max_storage_buffer_range: 1 << 25, max_push_constants_size: 16, ..panvk };
        let failed: Vec<&str> = scanner_limits(&small).iter().filter(|l| !l.3).map(|l| l.0).collect();
        assert_eq!(failed, ["maxStorageBufferRange", "maxPushConstantsSize"]);
        assert_eq!(SCAN_STORAGE_BUFFER_RANGE, 39_321_600);
        let edge = Limits { max_storage_buffer_range: SCAN_STORAGE_BUFFER_RANGE, ..panvk };
        assert!(scanner_limits(&edge)[0].3);
    }

    #[test]
    fn scanner_memory_needs_both_kinds() {
        let panvk = [DEVICE_LOCAL, DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT];
        assert_eq!(scanner_memory(&panvk), (true, true));
        assert_eq!(scanner_memory(&[DEVICE_LOCAL | HOST_VISIBLE]), (true, false));
        assert_eq!(scanner_memory(&[HOST_VISIBLE | HOST_COHERENT]), (false, true));
    }

    #[test]
    fn the_shader_is_spirv_with_a_main_entry_point() {
        let words = spirv_words(ADD_SPV).expect("SPIR-V magic");
        assert!(words.len() > 5);
        assert!(ADD_SPV.windows(5).any(|w| w == b"main\0"));
        assert!(spirv_words(b"\x01\x02\x03").is_none());
        assert!(spirv_words(&[0; 8]).is_none());
    }

    #[test]
    fn versions_read_like_vulkaninfo() {
        assert_eq!(api_version((1 << 22) | (4 << 12) | 354), "1.4.354");
        assert_eq!(api_version(1 << 22), "1.0.0");
    }

    #[test]
    fn memory_type_honours_the_allowed_bits_and_flags() {
        let host = 0x2 | 0x4; // HOST_VISIBLE | HOST_COHERENT
        let types = [0x1, 0x2, 0x1 | host];
        assert_eq!(memory_type(0b111, &types, host), Some(2));
        assert_eq!(memory_type(0b011, &types, host), None);
        assert_eq!(memory_type(0b001, &types, 0x1), Some(0));
    }

    #[test]
    fn a_wrong_element_is_found() {
        let (a, b) = inputs(1000);
        let mut c: Vec<u32> = a.iter().zip(&b).map(|(x, y)| x.wrapping_add(*y)).collect();
        assert_eq!(first_wrong(&a, &b, &c), None);
        c[512] ^= 1;
        assert_eq!(first_wrong(&a, &b, &c), Some(512));
        assert_eq!(first_wrong(&a, &b, &c[..10]), Some(0));
    }
}
