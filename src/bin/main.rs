#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::String;
use alloc::vec;
use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_backtrace as _;
use esp_hal::uart::{Config as UartConfig, DataBits, Parity, RxConfig, StopBits, Uart, UartInterrupt};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use esp_hal::{
    Blocking,
};
use rustpython_vm::VirtualMachine;
use core::cell::RefCell;

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

use critical_section::Mutex;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let p = esp_hal::init(config);

    //esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    //esp_alloc::heap_allocator!(size: 160 * 1024);
    esp_alloc::psram_allocator!(p.PSRAM, esp_hal::psram);

    esp_println::println!("Starting RustPython...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    let interpreter = rustpython_vm::Interpreter::without_stdlib(Default::default());

    esp_println::println!("Entering scope...");

    let scope = interpreter.enter(|vm| vm.new_scope_with_builtins());

    interpreter.enter(|vm| install_stdout(vm));

    esp_println::println!("Starting interpreter...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    // USB serial
    let mut usb = UsbSerialJtag::new(p.USB_DEVICE);

    loop {
        esp_println::print!(">>> ");
        let line = read_line(&mut usb);

        interpreter.enter(|vm| {
            let result = vm
                .compile(
                    &line,
                    rustpython_vm::compiler::Mode::Single,
                    alloc::string::String::from("<embedded>")
                )
                .map_err(|err| vm.new_syntax_error(&err, Some(&line)))
                .and_then(|code_obj| vm.run_code_obj(code_obj, scope.clone()));

            match result {
                Err(e) => {
                    let mut s = alloc::string::String::new();
                    vm.write_exception(&mut s, &e).unwrap();
                    esp_println::println!("Exception: {s}");
                }
                Ok(v) => {
                    if let Ok(s) = v.str(vm) {
                        esp_println::println!("{s}");
                    } else {
                        esp_println::println!("{v:?}");
                    }
                }
            }
        });
    }
}


fn read_line(usb: &mut UsbSerialJtag<'_, Blocking>) -> String {
    let mut line = String::new();
    'getline: loop {
        while let Ok(byte) = usb.read_byte() {
            match byte {
                // Backspace
                0x08 => {
                    if line.pop().is_some() {
                        usb.write(&[0x08, b' ', 0x08]);
                    }
                }
                // Other characters
                _ if byte.is_ascii() && !byte.is_ascii_control() => {
                    usb.write(&[byte]);
                    line.push(char::from(byte));
                }
                // Newlines
                b'\n' | b'\r' => {
                    usb.write(&[byte]);
                    break 'getline;
                }
                /*
                // Recall line
                0x1B => {
                    for _ in 0..line.len() {
                        usb.write(&[0x08, b' ', 0x08]);
                    }
                    core::mem::swap(&mut line, prev_line);
                    usb.write(line.as_bytes());
                },
                */
                _ => continue,
            }
        }
    }

    line
}

fn anon_object(vm: &VirtualMachine, name: &str) -> rustpython_vm::PyObjectRef {
    let py_type = vm.builtins.get_attr("type", vm).unwrap();
    let args = (name, vm.ctx.new_tuple(vec![]), vm.ctx.new_dict());
    py_type.call(args, vm).unwrap()
}


fn install_stdout(vm: &VirtualMachine) {
    let sys = vm.import("sys", 0).unwrap();

    let stdout = anon_object(vm, "InternalStdout");

    let writer = vm.new_function("write", move |s: String| esp_println::print!("{s}"));

    stdout.set_attr("write", writer, vm).unwrap();

    sys.set_attr("stdout", stdout.clone(), vm).unwrap();
}

