//! Kill-on-close job objects with a full UI lockdown. Form of
//! `srtwin/src_job.rs` (Apache-2.0): the sandboxed process tree dies with the
//! broker, and the child cannot touch the clipboard, global atoms, cross-job
//! USER/GDI handles, system parameters, display settings, the desktop or
//! ExitWindows. `breakaway_ok` is true only on the broker→runner job (so the
//! runner's child can break away onto its own load-bearing job), never on the
//! runner→child job.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use super::util::{OwnedHandle, ok};

/// An RAII job object. Its drop closes the handle, which with
/// `KILL_ON_JOB_CLOSE` terminates every process still in the job.
pub struct Job(OwnedHandle);

impl Job {
    /// A fresh unnamed job with kill-on-close and the UI lockdown.
    pub fn new(breakaway_ok: bool) -> anyhow::Result<Self> {
        // SAFETY: no name, no attributes.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            anyhow::bail!("CreateJobObjectW (os error {})", super::util::last_error());
        }
        let job = Self(OwnedHandle(handle));
        let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        ext.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if breakaway_ok {
            ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        // SAFETY: ext is the right size for JobObjectExtendedLimitInformation.
        ok(
            unsafe {
                SetInformationJobObject(
                    job.raw(),
                    JobObjectExtendedLimitInformation,
                    &ext as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            },
            "SetInformationJobObject(limits)",
        )?;
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_EXITWINDOWS,
        };
        // SAFETY: ui is the right size for JobObjectBasicUIRestrictions.
        ok(
            unsafe {
                SetInformationJobObject(
                    job.raw(),
                    JobObjectBasicUIRestrictions,
                    &ui as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                )
            },
            "SetInformationJobObject(ui)",
        )?;
        Ok(job)
    }

    pub fn raw(&self) -> HANDLE {
        self.0.raw()
    }

    /// Assigns a (suspended) process to the job.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn assign(&self, process: HANDLE) -> anyhow::Result<()> {
        // SAFETY: both handles are valid.
        ok(
            unsafe { AssignProcessToJobObject(self.raw(), process) },
            "AssignProcessToJobObject",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_with_ui_limits_is_created() {
        assert!(Job::new(false).is_ok());
        assert!(Job::new(true).is_ok());
    }
}
