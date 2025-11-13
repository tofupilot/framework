#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use tokio::sync::Mutex;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

#[cfg(windows)]
#[derive(Debug, Clone)]
struct JobHandle {
    handle: isize,
}

#[cfg(windows)]
impl JobHandle {
    fn new(handle: HANDLE) -> Self {
        Self {
            handle: handle.0 as isize,
        }
    }

    fn as_handle(&self) -> HANDLE {
        HANDLE(self.handle as *mut _)
    }
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct ProcessGroup {
    inner: Arc<Mutex<Option<JobHandle>>>,
}

#[cfg(not(windows))]
#[derive(Debug, Clone)]
pub struct ProcessGroup {}

impl ProcessGroup {
    pub fn new() -> Result<Self, String> {
        #[cfg(windows)]
        {
            unsafe {
                let job_handle = CreateJobObjectW(None, None)
                    .map_err(|e| format!("Failed to create job object: {:?}", e))?;

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                if let Err(e) = SetInformationJobObject(
                    job_handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) {
                    let _ = CloseHandle(job_handle);
                    return Err(format!("Failed to configure job object: {:?}", e));
                }

                Ok(Self {
                    inner: Arc::new(Mutex::new(Some(JobHandle::new(job_handle)))),
                })
            }
        }

        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    pub async fn add_process(&self, pid: u32) -> Result<(), String> {
        #[cfg(windows)]
        {
            let handle_guard = self.inner.lock().await;
            if let Some(ref job_handle) = *handle_guard {
                unsafe {
                    let process_handle =
                        OpenProcess(PROCESS_ALL_ACCESS, false, pid).map_err(|e| {
                            format!("Failed to open process {}: {:?}", pid, e)
                        })?;

                    let result = AssignProcessToJobObject(job_handle.as_handle(), process_handle);
                    let _ = CloseHandle(process_handle);

                    if let Err(e) = result {
                        return Err(format!(
                            "Failed to assign process {} to job object: {:?}",
                            pid, e
                        ));
                    }
                }
            }
        }

        #[cfg(not(windows))]
        {
            let _ = pid;
        }

        Ok(())
    }

    pub async fn kill_all(&self) -> Result<(), String> {
        #[cfg(windows)]
        {
            let handle_guard = self.inner.lock().await;
            if let Some(ref job_handle) = *handle_guard {
                unsafe {
                    TerminateJobObject(job_handle.as_handle(), 1)
                        .map_err(|e| format!("Failed to terminate job object: {:?}", e))?;
                }
            }
        }

        #[cfg(not(windows))]
        {
        }

        Ok(())
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.as_handle());
        }
    }
}
