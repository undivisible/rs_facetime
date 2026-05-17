use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn facetime_container_data() -> PathBuf {
    home_dir().join("Library/Containers/com.apple.FaceTime/Data")
}
