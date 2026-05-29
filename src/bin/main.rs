#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_backtrace as _;

extern crate alloc;

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let slice = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    esp_hal::rng::Rng::new().read(slice);
    Ok(())
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let _peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    esp_alloc::heap_allocator!(size: 165 * 1024);

    esp_println::println!("Starting RustPython...");

    let interpreter = rustpython_vm::Interpreter::without_stdlib(Default::default());

    esp_println::println!("Entering scope...");

    let scope = interpreter.enter(|vm| vm.new_scope_with_builtins());

    let source = alloc::string::String::from("6*7");

    esp_println::println!("Starting interpreter...");

    interpreter.enter(|vm| {
        let result = vm
            .compile(
                &source,
                rustpython_vm::compiler::Mode::Single,
                alloc::string::String::from("<embedded>")
            )
            .map_err(|err| vm.new_syntax_error(&err, Some(&source)))
            .and_then(|code_obj| vm.run_code_obj(code_obj, scope.clone()));

        match result {
            Err(e) => {
                let mut s = alloc::string::String::new();
                vm.write_exception(&mut s, &e).unwrap();
                esp_println::println!("Exception: {s}");
            }
            Ok(v) => {
                esp_println::println!("{v:?}");
            }
        }
    });

    esp_println::println!("Done. Looping forever.");

    loop {
        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(500) {}
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}
