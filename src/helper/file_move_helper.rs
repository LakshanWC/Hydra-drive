use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use super::log_helper::{save_log, read_uniqe_app_names, read_log};

fn show_console_logs(message:&str){
    print!("\r\x1b[2K{}", message);
    io::stdout().flush().unwrap();
}

pub fn move_file_to_destination(file_name: &str, file_path: &str, destination_path: &str) -> io::Result<()> {

    let mut full_destination = Path::new(destination_path).to_path_buf();
    full_destination.push(file_name);

    fs::rename(file_path, full_destination)?;
    Ok(())
}

//seperate method
pub fn move_file_or_directory(source: &str, destination:&str) -> io::Result<()>{
    fs::rename(source, destination)?;
    Ok(())
}

//save drive 
//does not work for single file

pub fn link_direcories(source: &str, destination:&str) -> io::Result<()>{
    let source_path = Path::new(source);
    let destination_path = Path::new(destination);

    if !source_path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "source is not a folder"));
    }
    if !destination_path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "destination is not a folder"));
    }

    let name = source_path.file_name().ok_or_else(||
        io::Error::new(io::ErrorKind::InvalidInput, "source has no folder name"))?;
    
    let final_path = destination_path.join(name);
        if final_path.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination folder already exists"));
    }
    
    match fs::rename(source_path, &final_path) {
        Ok(()) => {
            println!("same drive: moved with rename");
            // junction goes here next
            junction::create(&final_path, source_path)?;
            println!("junction created");
        }
        Err(e) if e.raw_os_error() == Some(17) => {
            println!("different drive: need copy, verify, swap");
            // slow path goes here later
        }
        Err(e) => return Err(e),
    }

    Ok(())

}

//work with different drives as well
//only 1 file
pub fn create_syslink(source: &str, destination: &str, application_name: &str) -> io::Result<()> {
    let source_path = Path::new(source);
    let destination_path = Path::new(destination);

    if !source_path.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "source is not a file"));
    }
    if !destination_path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "destination is not a directory"));
    }
    if application_name.is_empty(){
        return Err(io::Error::new(io::ErrorKind::InvalidInput,"Application Name can't be empty"));
    }

    // A missing log file just means nothing is logged yet.
    let uniqe_names = match read_uniqe_app_names() {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };

    if uniqe_names.is_empty() {
        println!("No apps logged yet.");
    }
    if uniqe_names.contains(&application_name.to_string()){
        println!("{} AlreadyExists there for use this", application_name);
    }

    let mut parts: Vec<_> = source_path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .rev()
        .take(2)          
        .collect();
    parts.reverse();  

    let relative_path:PathBuf = parts.into_iter().collect();

    let dest_file = destination_path.join(application_name).join(relative_path);


    if dest_file.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file already exists in destination path"));
    }

    show_console_logs("creating destination");
    if let Some(parent) = dest_file.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(source_path, &dest_file)?;

    show_console_logs("Verifing file");

    if fs::metadata(source_path)?.len() != fs::metadata(&dest_file)?.len() {
        let _ = fs::remove_file(&dest_file);
        return Err(io::Error::new(io::ErrorKind::Other, "copy verification failed"));
    }

    show_console_logs("renaming original source file");
    let mut back_up_name = source_path.file_name().unwrap().to_os_string();
    back_up_name.push(".org_back_up");
    let back_up = source_path.with_file_name(back_up_name);

    fs::rename(source_path, &back_up)?;

    show_console_logs("applying symlink");
    match std::os::windows::fs::symlink_file(&dest_file, source_path) {
        Ok(()) => {
            fs::remove_file(&back_up)?; // only now is the original really gone
            // Log only after the whole move succeeded, storing the full path of the moved file.
            save_log(source, &dest_file.to_string_lossy(), application_name)?;
            show_console_logs(&format!(
                "{} originally found here, file can be found here {}",
                source,
                dest_file.display()
            ));
            println!();
            Ok(())
        }
        Err(e) => {
            fs::rename(&back_up, source_path)?; // restore the original
            let _ = fs::remove_file(&dest_file);
            Err(e)
        }
    }
}

//only use try with 1 file for now
pub fn restore_linked_files() -> io::Result<()> {
    let data_points = match read_log() {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()), // nothing to restore
        Err(e) => return Err(e),
    };

    for item in &data_points {
        let original = Path::new(&item.original_location);
        let current = Path::new(&item.current_location);

        if !current.is_file() {
            eprintln!("Skipping {}: {} not found", item.application_name, current.display());
            continue;
        }

        // Remove the symlink at the original location (ignore if it's already gone).
        if original.symlink_metadata().is_ok() {
            fs::remove_file(original)?;
        }

        // Copy back (works across drives), verify, then delete the moved copy.
        fs::copy(current, original)?;
        if fs::metadata(current)?.len() != fs::metadata(original)?.len() {
            return Err(io::Error::new(io::ErrorKind::Other, "restore verification failed"));
        }
        fs::remove_file(current)?;
    }

    Ok(())
}