#![feature(lang_items)]
#![feature(const_fn)]
#![feature(alloc)]
#![feature(asm)]
#![feature(compiler_builtins_lib)]
#![no_std]
#![no_main]

#[macro_use]
extern crate stm32f7_discovery as stm32f7;

// initialization routines for .data and .bss

#[macro_use]
extern crate alloc;
extern crate compiler_builtins;
extern crate r0;

// hardware register structs with accessor methods
use stm32f7::{audio, board, embedded, ethernet, lcd, sdram, system_clock, touch, i2c, sd};
use stm32f7::ethernet::{TcpConnection, Udp};
use alloc::borrow::Cow;
#[no_mangle]
pub unsafe extern "C" fn reset() -> ! {
    extern "C" {
        static __DATA_LOAD: u32;
        static mut __DATA_END: u32;
        static mut __DATA_START: u32;

        static mut __BSS_START: u32;
        static mut __BSS_END: u32;
    }

    // initializes the .data section (copy the data segment initializers from flash to RAM)
    r0::init_data(&mut __DATA_START, &mut __DATA_END, &__DATA_LOAD);
    // zeroes the .bss section
    r0::zero_bss(&mut __BSS_START, &__BSS_END);

    stm32f7::heap::init();

    // enable floating point unit
    let scb = stm32f7::cortex_m::peripheral::scb_mut();
    scb.cpacr.modify(|v| v | 0b1111 << 20);
    asm!("DSB; ISB;"::::"volatile"); // pipeline flush

    main(board::hw());
}

// WORKAROUND: rust compiler will inline & reorder fp instructions into
#[inline(never)] //             reset() before the FPU is initialized
fn main(hw: board::Hardware) -> ! {
    use embedded::interfaces::gpio::{self, Gpio};
    use alloc::boxed::Box;

    hprintln!("Entering main");

    let x = vec![1, 2, 3, 4, 5];
    assert_eq!(x.len(), 5);
    assert_eq!(x[3], 4);

    let board::Hardware {
        rcc,
        pwr,
        flash,
        fmc,
        ltdc,
        gpio_a,
        gpio_b,
        gpio_c,
        gpio_d,
        gpio_e,
        gpio_f,
        gpio_g,
        gpio_h,
        gpio_i,
        gpio_j,
        gpio_k,
        i2c_3,
        sai_2,
        syscfg,
        ethernet_mac,
        ethernet_dma,
        nvic,
        exti,
        sdmmc,
        ..
    } = hw;

    let mut gpio = Gpio::new(
        gpio_a,
        gpio_b,
        gpio_c,
        gpio_d,
        gpio_e,
        gpio_f,
        gpio_g,
        gpio_h,
        gpio_i,
        gpio_j,
        gpio_k,
    );

    system_clock::init(rcc, pwr, flash);

    // enable all gpio ports
    rcc.ahb1enr.update(|r| {
        r.set_gpioaen(true);
        r.set_gpioben(true);
        r.set_gpiocen(true);
        r.set_gpioden(true);
        r.set_gpioeen(true);
        r.set_gpiofen(true);
        r.set_gpiogen(true);
        r.set_gpiohen(true);
        r.set_gpioien(true);
        r.set_gpiojen(true);
        r.set_gpioken(true);
    });

    // configure led pin as output pin
    let led_pin = (gpio::Port::PortI, gpio::Pin::Pin1);
    let mut led = gpio.to_output(
        led_pin,
        gpio::OutputType::PushPull,
        gpio::OutputSpeed::Low,
        gpio::Resistor::NoPull,
    ).expect("led pin already in use");

    // turn led on
    led.set(true);

    let button_pin = (gpio::Port::PortI, gpio::Pin::Pin11);
    let _ = gpio.to_input(button_pin, gpio::Resistor::NoPull)
        .expect("button pin already in use");

    // init sdram (needed for display buffer)
    sdram::init(rcc, fmc, &mut gpio);

    // lcd controller
    let mut lcd = lcd::init(ltdc, rcc, &mut gpio);
    let mut layer_1 = lcd.layer_1().unwrap();
    let mut layer_2 = lcd.layer_2().unwrap();

    layer_1.clear();
    layer_2.clear();
    lcd::init_stdout(layer_2);

    // i2c
    i2c::init_pins_and_clocks(rcc, &mut gpio);
    let mut i2c_3 = i2c::init(i2c_3);
    i2c_3.test_1();
    i2c_3.test_2();

    // sai and stereo microphone
    audio::init_sai_2_pins(&mut gpio);
    audio::init_sai_2(sai_2, rcc);
    assert!(audio::init_wm8994(&mut i2c_3).is_ok());

    // ethernet
    let mut eth_device = ethernet::EthernetDevice::new(
        Default::default(),
        Default::default(),
        rcc,
        syscfg,
        &mut gpio,
        ethernet_mac,
        ethernet_dma,
    );
    match eth_device {
        Ok(ref mut eth_device) => {
            eth_device
                .listen_on_udp_port(15, Box::new(udp_reverse))
                .unwrap();
            eth_device
                .register_tcp_port(15, Box::new(tcp_reverse))
                .unwrap();
        }
        Err(e) => println!("ethernet init failed: {:?}", e),
    }

    // SD
    let mut sd = sd::Sd::new(sdmmc, &mut gpio, rcc);

    touch::check_family_id(&mut i2c_3).unwrap();

    let mut audio_writer = layer_1.audio_writer();
    let mut last_led_toggle = system_clock::ticks();

    use stm32f7::board::embedded::interfaces::gpio::Port;
    use stm32f7::board::embedded::components::gpio::stm32f7::Pin;
    use stm32f7::exti::{EdgeDetection, Exti, ExtiLine};

    let mut exti = Exti::new(exti);
    let mut exti_handle = exti.register(
        ExtiLine::Gpio(Port::PortI, Pin::Pin11),
        EdgeDetection::FallingEdge,
        syscfg,
    ).unwrap();

    use stm32f7::interrupts::{scope, Priority};
    use stm32f7::interrupts::interrupt_request::InterruptRequest;

    scope(
        nvic,
        |_| {},
        |interrupt_table| {
            let _ = interrupt_table.register(InterruptRequest::Exti10to15, Priority::P1, move || {
                exti_handle.clear_pending_state();
                // choose a new background color
                let new_color = ((system_clock::ticks() as u32).wrapping_mul(19801)) % 0x1000000;
                lcd.set_background_color(lcd::Color::from_hex(new_color));
            });

            loop {
                let ticks = system_clock::ticks();

                // every 0.5 seconds
                if ticks - last_led_toggle >= 500 {
                    // toggle the led
                    let led_current = led.get();
                    led.set(!led_current);
                    last_led_toggle = ticks;
                }


                // poll for new touch data
                for touch in &touch::touches(&mut i2c_3).unwrap() {
                    audio_writer
                        .layer()
                        .print_point_at(touch.x as usize, touch.y as usize);
                }


                // handle new ethernet packets
                if let Ok(ref mut eth_device) = eth_device {
                    loop {
                        let result = eth_device.handle_next_packet();
                        if let Err(err) = result {
                            match err {
                                stm32f7::ethernet::Error::Exhausted => {}
                                _ => {} // println!("err {:?}", e),
                            }
                            break;
                        }
                    }
                }

                // Initialize the SD Card on insert and deinitialize on extract.
                if sd.card_present() && !sd.card_initialized() {
                    if let Some(i_err) = sd::init(&mut sd).err() {
                        hprintln!("{:?}", i_err);
                    }
                } else if !sd.card_present() && sd.card_initialized() {
                    sd::de_init(&mut sd);
                }
            }
        },
    )
}

fn udp_reverse(udp: Udp) -> Option<Cow<[u8]>> {
    for byte in udp.payload.iter().filter(|&&b| b != 0) {
        print!("{}", char::from(*byte));
    }
    let mut reply = b"Reversed: ".to_vec();
    let start = reply.len();
    reply.extend_from_slice(udp.payload);
    let end = reply.len() - 1;
    reply[start..end].reverse();
    Some(reply.into())
}

fn tcp_reverse<'a>(_connection: &TcpConnection, data: &'a [u8]) -> Option<Cow<'a, [u8]>> {
    for byte in data.iter().filter(|&&b| b != 0) {
        print!("{}", char::from(*byte));
    }
    if data.len() > 0 {
        let mut reply = b"Reversed: ".to_vec();
        let start = reply.len();
        reply.extend_from_slice(data);
        let end = reply.len() - 1;
        reply[start..end].reverse();
        Some(reply.into())
    } else {
        None
    }
}
