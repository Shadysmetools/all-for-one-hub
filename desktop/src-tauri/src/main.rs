#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `one-desktop --store-key` reads the ElevenLabs key from stdin into the OS keychain.
    if std::env::args().nth(1).as_deref() == Some("--store-key") {
        match one_desktop_lib::store_key_from_stdin() {
            Ok(()) => println!("Key stored in the OS keychain."),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    one_desktop_lib::run();
}
