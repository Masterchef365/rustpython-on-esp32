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
use esp_hal::uart::{Config as UartConfig, DataBits, Parity, RxConfig, StopBits, Uart, UartInterrupt};
use esp_hal::{
    Blocking,
};
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

static SERIAL: Mutex<RefCell<Option<Uart<Blocking>>>> = Mutex::new(RefCell::new(None));

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
    let p = esp_hal::init(config);

    //esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    esp_alloc::heap_allocator!(size: 160 * 1024);
    esp_alloc::psram_allocator!(p.PSRAM, esp_hal::psram);

    esp_println::println!("Starting RustPython...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    let interpreter = rustpython_vm::Interpreter::without_stdlib(Default::default());

    esp_println::println!("Entering scope...");

    let scope = interpreter.enter(|vm| vm.new_scope_with_builtins());

    let source = alloc::string::String::from("6*7");

    esp_println::println!("Starting interpreter...");
    esp_println::println!("{}", esp_alloc::HEAP.stats());

    let (tx_pin, rx_pin) = (p.GPIO43, p.GPIO44);

    let uart_config = UartConfig::default()
        .with_baudrate(19200)
        .with_data_bits(DataBits::_8)
        .with_parity(Parity::None)
        .with_stop_bits(StopBits::_1)
        .with_rx(RxConfig::default().with_fifo_full_threshold(1));
    let mut uart = Uart::new(p.UART1, uart_config)
        .expect("Failed to initialize UART")
        .with_tx(tx_pin)
        .with_rx(rx_pin);

    uart.set_interrupt_handler(handler);

    critical_section::with(|cs| {
        uart.clear_interrupts(UartInterrupt::RxBreakDetected | UartInterrupt::RxFifoFull);
        uart.listen(UartInterrupt::RxBreakDetected | UartInterrupt::RxFifoFull);
        SERIAL.borrow_ref_mut(cs).replace(uart);
    });

    loop {
        interpreter.enter(|vm| {
            let result = vm
                .compile(
                    &source,
                    rustpython_vm::compiler::Mode::Eval,
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

        esp_println::println!("{}", esp_alloc::HEAP.stats());

        //esp_println::println!("Done. Looping forever.");

        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(5000) {}
    }
}

#[esp_hal::handler]
//#[esp_hal::ram]
fn handler() {
    critical_section::with(|cs| {
        let mut serial = SERIAL.borrow_ref_mut(cs);
        let serial = serial.as_mut().unwrap();

        if serial.interrupts().contains(UartInterrupt::RxBreakDetected) {
            esp_println::print!("\nBREAK");
        }
        if serial.interrupts().contains(UartInterrupt::RxFifoFull) {
            let mut byte = [0u8; 1];
            serial.read(&mut byte).unwrap();
            esp_println::print!(" {:02X}", byte[0]);
        }

        serial.clear_interrupts(UartInterrupt::RxBreakDetected | UartInterrupt::RxFifoFull);
    });
}
