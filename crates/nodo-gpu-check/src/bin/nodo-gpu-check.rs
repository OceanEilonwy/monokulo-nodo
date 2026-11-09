//! Lists Vulkan devices, then adds two buffers of integers with a compute shader on the first
//! one that has a compute queue, checks every element and prints how long it took.
//!
//! Exit status: 0 when the result is right, 1 when it is wrong or Vulkan fails, 2 when there is
//! no Vulkan loader or no device. Read-only and self-contained, so nodo-report runs it.

use std::ffi::CStr;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ash::vk;
use nodo_gpu_check::{
    api_version, first_wrong, inputs, memory_type, scanner_limits, scanner_memory, spirv_words, Limits, ADD_SPV,
    DEVICE_LOCAL, HOST_COHERENT, HOST_VISIBLE, WORKGROUP,
};

/// 4 Mi elements, 16 MiB per buffer: long enough to time, small enough for any device.
const ELEMENTS: u32 = 4 << 20;
const FENCE_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("usage: nodo-gpu-check\nLists Vulkan devices and runs a small compute shader on the first one.");
        return ExitCode::SUCCESS;
    }
    // SAFETY: loading the system's Vulkan loader runs its initialisers, as any Vulkan program does.
    let entry = match unsafe { ash::Entry::load() } {
        Ok(entry) => entry,
        Err(e) => {
            println!("no Vulkan loader: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&entry) {
        Ok(code) => code,
        Err(e) => {
            println!("FAILED: {e}");
            ExitCode::from(1)
        }
    }
}

type Res<T> = Result<T, String>;

fn vkerr(what: &'static str) -> impl Fn(vk::Result) -> String {
    move |e| format!("{what}: {e}")
}

fn run(entry: &ash::Entry) -> Res<ExitCode> {
    // SAFETY: plain Vulkan calls with valid create infos; every object is destroyed before return.
    unsafe {
        let loader = entry.try_enumerate_instance_version().ok().flatten().unwrap_or(vk::API_VERSION_1_0);
        println!("Vulkan loader {}", api_version(loader));
        let app = vk::ApplicationInfo::default()
            .application_name(c"nodo-gpu-check")
            .api_version(vk::API_VERSION_1_1);
        let instance = entry
            .create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)
            .map_err(vkerr("creating the instance"))?;
        let result = with_instance(&instance);
        instance.destroy_instance(None);
        result
    }
}

unsafe fn with_instance(instance: &ash::Instance) -> Res<ExitCode> {
    // With drivers installed but none finding its hardware, the loader may answer with an error
    // instead of an empty list (it says VK_ERROR_INITIALIZATION_FAILED for panvk without a Mali).
    let devices = match instance.enumerate_physical_devices() {
        Ok(devices) => devices,
        Err(e) => {
            println!("no Vulkan device (listing devices: {e})");
            return Ok(ExitCode::from(2));
        }
    };
    if devices.is_empty() {
        println!("no Vulkan device");
        return Ok(ExitCode::from(2));
    }
    let mut chosen = None;
    for (index, &pd) in devices.iter().enumerate() {
        let p = instance.get_physical_device_properties(pd);
        let name = p.device_name_as_c_str().map(CStr::to_string_lossy).unwrap_or_default();
        println!(
            "device {index}: {name} ({}), Vulkan {}, driver version {:#x}, vendor {:#06x} device {:#06x}",
            device_type(p.device_type),
            api_version(p.api_version),
            p.driver_version,
            p.vendor_id,
            p.device_id
        );
        if p.api_version >= vk::API_VERSION_1_2 {
            let mut driver = vk::PhysicalDeviceDriverProperties::default();
            let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
            instance.get_physical_device_properties2(pd, &mut p2);
            let driver_name = driver.driver_name_as_c_str().map(CStr::to_string_lossy).unwrap_or_default();
            let info = driver.driver_info_as_c_str().map(CStr::to_string_lossy).unwrap_or_default();
            let c = driver.conformance_version;
            println!(
                "  driver {driver_name} {info}, conformance {}.{}.{}.{}",
                c.major, c.minor, c.subminor, c.patch
            );
        }
        let families = instance.get_physical_device_queue_family_properties(pd);
        let compute = families.iter().position(|f| f.queue_flags.contains(vk::QueueFlags::COMPUTE));
        println!("  compute queue family: {}", compute.map_or("none".to_owned(), |q| q.to_string()));
        if chosen.is_none() {
            if let Some(q) = compute {
                chosen = Some((index, pd, q as u32));
            }
        }
    }
    let Some((index, pd, family)) = chosen else {
        println!("no device with a compute queue");
        return Ok(ExitCode::from(2));
    };
    let scanner_ok = scanner_requirements(instance, pd);
    println!("compute test on device {index}: c = a + b over {ELEMENTS} integers");
    let priorities = [1.0];
    let queue_info = vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities);
    let device = instance
        .create_device(pd, &vk::DeviceCreateInfo::default().queue_create_infos(std::slice::from_ref(&queue_info)), None)
        .map_err(vkerr("creating the device"))?;
    let memory = instance.get_physical_device_memory_properties(pd);
    let result = compute(&device, &memory, family);
    device.destroy_device(None);
    match result {
        Ok(code) if code == ExitCode::SUCCESS && !scanner_ok => {
            println!("the compute test passed, but the wallet scanner's requirements are not met");
            Ok(ExitCode::from(1))
        }
        other => other,
    }
}

/// Prints the limits and memory the Monokulo wallet scanner (monokulo-8ar) checks, against its
/// thresholds, and whether the device meets all of them.
unsafe fn scanner_requirements(instance: &ash::Instance, pd: vk::PhysicalDevice) -> bool {
    let p = instance.get_physical_device_properties(pd);
    let l = &p.limits;
    let limits = Limits {
        max_storage_buffer_range: l.max_storage_buffer_range,
        max_compute_work_group_invocations: l.max_compute_work_group_invocations,
        max_compute_work_group_size_x: l.max_compute_work_group_size[0],
        max_push_constants_size: l.max_push_constants_size,
        max_per_stage_descriptor_storage_buffers: l.max_per_stage_descriptor_storage_buffers,
    };
    println!("wallet scanner (monokulo-8ar) requirements:");
    let mut ok = true;
    for (name, has, needs, met) in scanner_limits(&limits) {
        println!("  {} {name} = {has} (needs >= {needs})", if met { "PASS" } else { "FAIL" });
        ok &= met;
    }
    let memory = instance.get_physical_device_memory_properties(pd);
    let types: Vec<u32> = memory.memory_types_as_slice().iter().map(|t| t.property_flags.as_raw()).collect();
    for (i, t) in types.iter().enumerate() {
        let mut names = Vec::new();
        for (bit, name) in [(DEVICE_LOCAL, "DEVICE_LOCAL"), (HOST_VISIBLE, "HOST_VISIBLE"), (HOST_COHERENT, "HOST_COHERENT"), (0x8, "HOST_CACHED")] {
            if t & bit != 0 {
                names.push(name);
            }
        }
        println!("  memory type {i}: {}", if names.is_empty() { "none".to_owned() } else { names.join(" | ") });
    }
    let (local, coherent) = scanner_memory(&types);
    println!("  {} a DEVICE_LOCAL memory type", if local { "PASS" } else { "FAIL" });
    println!("  {} a HOST_VISIBLE | HOST_COHERENT memory type", if coherent { "PASS" } else { "FAIL" });
    let gpu = matches!(p.device_type, vk::PhysicalDeviceType::DISCRETE_GPU | vk::PhysicalDeviceType::INTEGRATED_GPU);
    println!("  {} a discrete or integrated GPU ({})", if gpu { "PASS" } else { "FAIL" }, device_type(p.device_type));
    ok &= local && coherent && gpu;
    println!("wallet scanner requirements: {}", if ok { "all met" } else { "NOT met" });
    ok
}

fn device_type(t: vk::PhysicalDeviceType) -> &'static str {
    match t {
        vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated GPU",
        vk::PhysicalDeviceType::DISCRETE_GPU => "discrete GPU",
        vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual GPU",
        vk::PhysicalDeviceType::CPU => "CPU",
        _ => "other",
    }
}

struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u32,
}

unsafe fn buffer(device: &ash::Device, memory: &vk::PhysicalDeviceMemoryProperties) -> Res<Buffer> {
    let size = u64::from(ELEMENTS) * 4;
    let buffer = device
        .create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )
        .map_err(vkerr("creating a buffer"))?;
    let req = device.get_buffer_memory_requirements(buffer);
    let types: Vec<u32> = memory.memory_types_as_slice().iter().map(|t| t.property_flags.as_raw()).collect();
    let wanted = (vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT).as_raw();
    let Some(type_index) = memory_type(req.memory_type_bits, &types, wanted) else {
        device.destroy_buffer(buffer, None);
        return Err("no host-visible coherent memory".into());
    };
    let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(type_index);
    let mem = match device.allocate_memory(&alloc, None) {
        Ok(m) => m,
        Err(e) => {
            device.destroy_buffer(buffer, None);
            return Err(format!("allocating memory: {e}"));
        }
    };
    let mapped = device
        .bind_buffer_memory(buffer, mem, 0)
        .and_then(|()| device.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()));
    match mapped {
        Ok(ptr) => Ok(Buffer { buffer, memory: mem, ptr: ptr.cast() }),
        Err(e) => {
            device.destroy_buffer(buffer, None);
            device.free_memory(mem, None);
            Err(format!("mapping memory: {e}"))
        }
    }
}

unsafe fn compute(device: &ash::Device, memory: &vk::PhysicalDeviceMemoryProperties, family: u32) -> Res<ExitCode> {
    let mut buffers = Vec::new();
    let result = (|| -> Res<ExitCode> {
        for _ in 0..3 {
            buffers.push(buffer(device, memory)?);
        }
        let n = ELEMENTS as usize;
        let (a, b) = inputs(ELEMENTS);
        std::ptr::copy_nonoverlapping(a.as_ptr(), buffers[0].ptr, n);
        std::ptr::copy_nonoverlapping(b.as_ptr(), buffers[1].ptr, n);
        std::ptr::write_bytes(buffers[2].ptr, 0, n);
        let handles: Vec<vk::Buffer> = buffers.iter().map(|b| b.buffer).collect();
        let times = dispatch(device, family, &handles, 3)?;
        let c = std::slice::from_raw_parts(buffers[2].ptr, n);
        let cpu_start = Instant::now();
        let reference: Vec<u32> = a.iter().zip(&b).map(|(x, y)| x.wrapping_add(*y)).collect();
        let cpu = cpu_start.elapsed();
        std::hint::black_box(&reference);
        let ms = |d: &Duration| format!("{:.2} ms", d.as_secs_f64() * 1000.0);
        let runs: Vec<String> = times.iter().map(ms).collect();
        match first_wrong(&a, &b, c) {
            None => {
                println!("compute result: ok, all {ELEMENTS} elements right");
                println!("GPU time per run (submit to fence): {}", runs.join(", "));
                println!("CPU time for the same additions, one core: {}", ms(&cpu));
                Ok(ExitCode::SUCCESS)
            }
            Some(i) => {
                println!(
                    "compute result: WRONG at element {i}: {} + {} gave {}",
                    a[i], b[i], c.get(i).copied().unwrap_or_default()
                );
                Ok(ExitCode::from(1))
            }
        }
    })();
    for b in buffers {
        device.unmap_memory(b.memory);
        device.destroy_buffer(b.buffer, None);
        device.free_memory(b.memory, None);
    }
    result
}

/// Builds the pipeline and runs it `runs` times, returning each run's time.
unsafe fn dispatch(device: &ash::Device, family: u32, buffers: &[vk::Buffer], runs: usize) -> Res<Vec<Duration>> {
    let words = spirv_words(ADD_SPV).ok_or("the built-in shader is not SPIR-V")?;
    let module = device
        .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        .map_err(vkerr("creating the shader"))?;
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..3)
        .map(|i| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let set_layout = device
        .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
        .map_err(vkerr("creating the descriptor layout"))?;
    let layouts = [set_layout];
    let pipeline_layout = device
        .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts), None)
        .map_err(vkerr("creating the pipeline layout"))?;
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(module)
        .name(c"main");
    let started = Instant::now();
    let pipeline = device
        .create_compute_pipelines(
            vk::PipelineCache::null(),
            &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(pipeline_layout)],
            None,
        )
        .map_err(|(_, e)| format!("creating the pipeline: {e}"))?[0];
    println!("pipeline compiled in {:.2} ms", started.elapsed().as_secs_f64() * 1000.0);
    let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 3 }];
    let pool = device
        .create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None)
        .map_err(vkerr("creating the descriptor pool"))?;
    let set = device
        .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts))
        .map_err(vkerr("allocating the descriptor set"))?[0];
    let infos: Vec<vk::DescriptorBufferInfo> =
        buffers.iter().map(|&b| vk::DescriptorBufferInfo::default().buffer(b).offset(0).range(vk::WHOLE_SIZE)).collect();
    let writes: Vec<vk::WriteDescriptorSet> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(i as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(info))
        })
        .collect();
    device.update_descriptor_sets(&writes, &[]);
    let command_pool = device
        .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family), None)
        .map_err(vkerr("creating the command pool"))?;
    let command = device
        .allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
        .map_err(vkerr("allocating the command buffer"))?[0];
    device.begin_command_buffer(command, &vk::CommandBufferBeginInfo::default()).map_err(vkerr("recording"))?;
    device.cmd_bind_pipeline(command, vk::PipelineBindPoint::COMPUTE, pipeline);
    device.cmd_bind_descriptor_sets(command, vk::PipelineBindPoint::COMPUTE, pipeline_layout, 0, &[set], &[]);
    device.cmd_dispatch(command, ELEMENTS / WORKGROUP, 1, 1);
    let barrier =
        vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::HOST_READ);
    device.cmd_pipeline_barrier(
        command,
        vk::PipelineStageFlags::COMPUTE_SHADER,
        vk::PipelineStageFlags::HOST,
        vk::DependencyFlags::empty(),
        &[barrier],
        &[],
        &[],
    );
    device.end_command_buffer(command).map_err(vkerr("recording"))?;
    let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(vkerr("creating a fence"))?;
    let queue = device.get_device_queue(family, 0);
    let commands = [command];
    let mut times = Vec::new();
    let mut failure = None;
    for _ in 0..runs {
        let start = Instant::now();
        let done = device
            .queue_submit(queue, &[vk::SubmitInfo::default().command_buffers(&commands)], fence)
            .and_then(|()| device.wait_for_fences(&[fence], true, FENCE_TIMEOUT.as_nanos() as u64))
            .and_then(|()| device.reset_fences(&[fence]));
        if let Err(e) = done {
            failure = Some(format!("running the shader: {e}"));
            break;
        }
        times.push(start.elapsed());
    }
    let _ = device.device_wait_idle();
    device.destroy_fence(fence, None);
    device.destroy_command_pool(command_pool, None);
    device.destroy_descriptor_pool(pool, None);
    device.destroy_pipeline(pipeline, None);
    device.destroy_pipeline_layout(pipeline_layout, None);
    device.destroy_descriptor_set_layout(set_layout, None);
    device.destroy_shader_module(module, None);
    match failure {
        Some(e) => Err(e),
        None => Ok(times),
    }
}
