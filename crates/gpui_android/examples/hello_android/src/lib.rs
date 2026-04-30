#![cfg(target_os = "android")]

use android_activity::{AndroidApp, MainEvent, PollEvent};
use log::info;

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Trace)
            .with_tag("hello_android"),
    );
    info!("android_main: bootstrap, entering event loop");

    let mut quit = false;
    while !quit {
        app.poll_events(Some(std::time::Duration::from_millis(500)), |event| {
            match event {
                PollEvent::Wake => info!("event: Wake"),
                PollEvent::Timeout => info!("event: Timeout"),
                PollEvent::Main(main_event) => {
                    info!("main event: {main_event:?}");
                    if matches!(main_event, MainEvent::Destroy) {
                        quit = true;
                    }
                }
                _ => {}
            }
        });
    }
    info!("android_main: exiting");
}
