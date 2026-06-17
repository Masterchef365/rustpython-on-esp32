#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::String;
use alloc::string::ToString;
use alloc::{format, vec};
use core::cell::RefCell;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_hal::uart::{
    Config as UartConfig, DataBits, Parity, RxConfig, StopBits, Uart, UartInterrupt,
};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use esp_hal::Blocking;
use rustpython_vm::convert::IntoObject;
use rustpython_vm::scope::Scope;
use rustpython_vm::VirtualMachine;

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

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64000);
    esp_alloc::psram_allocator!(p.PSRAM, esp_hal::psram);

    esp_println::println!("Starting RustPython...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    let interpreter = rustpython_vm::Interpreter::without_stdlib(Default::default());

    esp_println::println!("Entering scope...");

    let scope = interpreter.enter(|vm| vm.new_scope_with_builtins());

    interpreter.enter(|vm| {
        install_meminfo_fn(vm, &scope);
        install_stdout(vm);
    });

    esp_println::println!("Starting interpreter...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    // USB serial
    let mut usb = UsbSerialJtag::new(p.USB_DEVICE);

    let mut prev_line = String::new();

    loop {
        esp_println::print!(">>> ");
        let line = read_line(&mut usb, &mut prev_line);
        let command_line = parse_command_line(&line);

        let source_path = "<embedded>";
        let mode = rustpython_vm::compiler::Mode::Single;
        interpreter.enter(|vm| {
            let result = command_line
                .ok_or_else(|| "No command".to_string())
                .and_then(|command_line| {
                    vm.compile(
                        &command_line,
                        mode,
                        alloc::string::String::from(source_path),
                    )
                    .map_err(|err| vm.new_syntax_error(&err, Some(&command_line)))
                    .map_err(|e| {
                        let mut s = alloc::string::String::new();
                        vm.write_exception(&mut s, &e).unwrap();
                        s
                    })
                })
                .or_else(|other_error| {
                    vm.compile(&line, mode, alloc::string::String::from(source_path))
                        .map_err(|err| vm.new_syntax_error(&err, Some(&line)))
                        .map_err(|e| {
                            let mut s = alloc::string::String::new();
                            vm.write_exception(&mut s, &e).unwrap();
                            format!("{s}\n{other_error}")
                        })
                });

            let result = result.and_then(|code_obj| {
                vm.run_code_obj(code_obj, scope.clone()).map_err(|e| {
                    let mut s = alloc::string::String::new();
                    vm.write_exception(&mut s, &e).unwrap();
                    format!("{s}")
                })
            });

            match result {
                Err(e) => {
                    esp_println::println!("Exception: {e}");
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

        prev_line = line;
    }
}

fn read_line(usb: &mut UsbSerialJtag<'_, Blocking>, prev_line: &mut String) -> String {
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
                    usb.write(b"\n");
                    break 'getline;
                }
                // Escape codes
                0x1B => match [usb.read_byte(), usb.read_byte()] {
                    [Ok(b'['), Ok(b'A' | b'B')] => {
                        for _ in 0..line.len() {
                            usb.write(&[0x08, b' ', 0x08]);
                        }
                        core::mem::swap(&mut line, prev_line);
                        usb.write(line.as_bytes());
                    }
                    _ => {}
                },
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

fn install_meminfo_fn(vm: &VirtualMachine, scope: &Scope) {
    let heapstats = vm.new_function("heapstats", move || {
        esp_println::print!("{}", esp_alloc::HEAP.stats())
    });

    scope
        .globals
        .set_item("heapstats", heapstats.into_object(), vm);
}

/// Attempt to assemble a function call string, from a 'command line'
/// style syntax.
/// This will turn the string 'run_my_code "6 " 7' into 'run_my_code("6 ", 7)'
/// for example.
fn parse_command_line(line: &str) -> Option<String> {
    let (func_name, xs) = line.split_once(char::is_whitespace)?;

    let mut double_quote = false;
    let mut single_quote = false;
    let mut backslash = false;

    let mut args = vec![];
    let mut current_arg = String::new();

    for c in xs.chars() {
        match c {
            '\\' => {
                if backslash {
                    current_arg.push(c);
                }
                backslash = !backslash;
            }
            '"' => {
                current_arg.push(c);
                if !(backslash | single_quote) {
                    double_quote = !double_quote;
                }
                backslash = false;
            }
            '\'' => {
                current_arg.push(c);
                if !(backslash | double_quote) {
                    single_quote = !single_quote;
                }
                backslash = false;
            }
            ' ' => {
                if !(backslash | double_quote | single_quote) {
                    args.push(core::mem::take(&mut current_arg));
                } else {
                    current_arg.push(c);
                }
                backslash = false
            }
            _ => {
                current_arg.push(c);
                backslash = false;
            }
        }
    }

    if !current_arg.is_empty() {
        args.push(current_arg);
    }

    let mut call = format!("{func_name}(");

    for (idx, arg) in args.iter().enumerate() {
        call.push_str(&arg);
        if idx + 1 != args.len() {
            call.push_str(", ");
        }
    }

    call.push(')');

    Some(call)
}

#[cfg(test)]
#[test]
fn test_parse_command_line_1() {
    assert_eq!(
        parse_command_line("a 'b' 'c'"),
        Some("a('b', 'c')".to_string())
    );
    assert_eq!(
        parse_command_line("afunction 'bar' 5"),
        Some("afunction('bar', 5)".to_string())
    );
}

#[cfg(test)]
#[test]
fn test_parse_command_line_2() {
    assert_eq!(
        parse_command_line("afunction 'bar\"' 5"),
        Some("afunction('bar\"', 5)".to_string())
    );
    assert_eq!(
        parse_command_line("afunction \"bar'\" 5"),
        Some("afunction(\"bar'\", 5)".to_string())
    );
    assert_eq!(
        parse_command_line("afunction \"'bar'\" 5"),
        Some("afunction(\"'bar'\", 5)".to_string())
    );
    assert_eq!(
        parse_command_line("afunction '\"bar\"' 5"),
        Some("afunction('\"bar\"', 5)".to_string())
    );
}

#[cfg(test)]
#[test]
fn test_parse_command_line_3() {
    assert_eq!(
        parse_command_line("afunction 'bar \"' 5"),
        Some("afunction('bar \"', 5)".to_string())
    );
    assert_eq!(
        parse_command_line("afunction 'bar \" no bueno \"' 5"),
        Some("afunction('bar \" no bueno \"', 5)".to_string())
    );
    assert_eq!(
        parse_command_line("afunction 'bar, \" no bueno \"' 5"),
        Some("afunction('bar, \" no bueno \"', 5)".to_string())
    );
    // We are supposed to create a parse error when the other way is used
    assert_eq!(
        parse_command_line("afunction('bar, \" no bueno \"', 5)"),
        Some("afunction('bar,(\" no bueno \"', 5))".to_string())
    );
}
