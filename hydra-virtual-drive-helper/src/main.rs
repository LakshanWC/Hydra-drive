use std::env;
use std::io;
use std::sync::atomic::Ordering;
use hydra_drive_core::ipc;

fn pause() {
    println!("\nPress Enter to close...");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
}

fn main() {
    let raw: Vec<String> = env::args().collect();
    println!("raw: {:?}", raw);

    let args = match ipc::parse_and_validate_args(env::args().skip(1)) {
        Ok(a) => a,
        Err(code) => {
            println!("bad args, exit code {}", code);
            pause();
            std::process::exit(code as i32);
        }
    };

    println!("parsed: {:#?}", args);
    print!("validating flag {:#?}",hydra_drive_core::CREATE_PREVILAGE_VIRTUAL_DRIVE);
    println!("Creating admin level virtual drive");

    if hydra_drive_core::CREATE_PREVILAGE_VIRTUAL_DRIVE.load(Ordering::Relaxed)
    {
        let drive = match hydra_drive_core::create_virtual_drive(hydra_drive_core::selected_roots()) {
            Ok(d) => d, // keep the drive; dropping it unmounts it
            Err(e) => {
                println!("mount failed: {e}");
                pause();
                std::process::exit(ipc::EXIT_MOUNT_FAILED as i32);
            }
        };

        println!("Admin level virtual drive created");
        pause(); // drive stays mounted while we wait here

        drop(drive); // unmounts
    }

}