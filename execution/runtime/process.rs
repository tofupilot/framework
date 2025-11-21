pub fn configure_no_window(_cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        _cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
}

pub fn configure_no_window_tokio(_cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        _cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
}
