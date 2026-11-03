use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::{env, path::PathBuf};

#[derive(Serialize, Deserialize, Debug)]
pub struct Data {
    pub application_name: String,
    pub current_location: String,
    pub original_location: String,
}

fn data_path() -> io::Result<PathBuf> {
    let mut path = env::current_exe()?;
    path.set_file_name("data.json");
    Ok(path)
}

pub fn save_log(source_path: &str, destination_path: &str, application_name: &str) -> io::Result<()> {
    // Load existing entries; a missing file just means an empty log.
    let mut entries = match read_log() {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };

    entries.push(Data {
        application_name: application_name.to_string(),
        current_location: destination_path.to_string(),
        original_location: source_path.to_string(),
    });

    let file = File::create(data_path()?)?;
    let mut writer = BufWriter::new(file);

    serde_json::to_writer_pretty(&mut writer, &entries)?;

    writer.flush()?;
    Ok(())
}

pub fn read_log() -> io::Result<Vec<Data>> {
    let file = File::open(data_path()?)?;
    let reader = BufReader::new(file);
    let entries: Vec<Data> = serde_json::from_reader(reader)?;
    Ok(entries)
}

pub fn read_uniqe_app_names() -> io::Result<Vec<String>> {
    let data_points = read_log()?;
    let mut uniqe_app_names: Vec<String> = Vec::new();

    for item in &data_points {
        if !uniqe_app_names.contains(&item.application_name) {
            uniqe_app_names.push(item.application_name.clone());
        }
    }

    Ok(uniqe_app_names)
}