fn host_cfg_macro() -> u8 {
    if cfg!(unix) { 1 } else { 2 }
}

#[cfg(target_os = "linux")]
fn host_cfg_attr() {}

#[cfg_attr(windows, allow(dead_code))]
fn host_cfg_attr_conditional() {}

std::cfg_select! {
    target_os = "macos" => { fn host_select() {} }
    _ => { fn host_select() {} }
}

fn main() {}
