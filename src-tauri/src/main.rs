// Prevents an additional console window from appearing in release mode on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    playlist_curator_lib::run();
}
