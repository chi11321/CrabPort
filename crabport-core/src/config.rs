use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug)]
pub enum CrabPortColorConfig {}

#[derive(Clone, Debug)]
pub struct CrabPortFontConfig {
    font_size: f32,
    font_family: String,
    height: f32,
    weith: f32,
}

#[derive(Clone, Debug)]
pub struct CrabPortProxyConfig {
    url: Arc<String>,
    timeout: u64,
}

#[derive(Clone, Debug)]
pub enum CrabPortKeybindPrefix {
    Command,
    Control,
    Alt,
}

#[derive(Clone, Debug)]
pub struct CrabPortConfig {
    color: CrabPortColorConfig,
    font: CrabPortFontConfig,
    proxy: CrabPortProxyConfig,
    keybind: HashMap<(CrabPortKeybindPrefix, u8), String>,
}
