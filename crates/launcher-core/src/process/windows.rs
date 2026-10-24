//! Windows process control: a job object per zebrad, and the liveness and
//! executable checks behind the pid file.
//!
//! The child is created suspended, assigned to a fresh job, and then resumed,
//! so nothing it starts can escape the job. Terminating the job kills the
//! whole tree. The launcher itself is never put in a job: that would also kill
//! the installer the updater runs and any browser the launcher opens.

use std::{
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::PathBuf,
};

use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, STILL_ACTIVE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetExitCodeProcess, OpenProcess, OpenThread,
            PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
            QueryFullProcessImageNameW, ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess,
        },
    },
};

/// Hides the console window and starts the process suspended, so it can be
/// placed in its job before it runs.
pub(super) fn prepare(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
}

/// A job object that owns zebrad and its descendants.
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
}

impl Job {
    /// Creates an anonymous job. With `kill_on_close`, closing the last handle
    /// (for example because the launcher crashed) kills every process in it.
    pub fn new(kill_on_close: bool) -> io::Result<Self> {
        // SAFETY: null attributes and name are documented as valid arguments.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the handle was just created and is owned by nobody else.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if kill_on_close {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .expect("the limit struct is a few dozen bytes");
        // SAFETY: the handle is a valid job and `limits` is the struct the
        // information class expects, with its exact size.
        let ok = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Adds a process to the job.
    pub fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: both handles are valid for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Kills every process in the job.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: the handle is a valid job we own.
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Returns true when a process with `pid` exists and has not exited.
pub(super) fn is_alive(pid: u32) -> bool {
    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut exit_code: u32 = 0;
    // SAFETY: the handle is valid for the call and exit_code points to writable memory.
    let ok = unsafe { GetExitCodeProcess(handle.as_raw_handle(), &mut exit_code) };
    // STILL_ACTIVE is 259; a process that happens to exit with 259 is misreported, which
    // is the documented limitation of this API and acceptable for a stale-pid hint.
    ok != 0 && exit_code == STILL_ACTIVE as u32
}

/// Terminates the process `pid` outright (`TerminateProcess`): for a stale
/// zebrad, which is in no job the launcher owns.
pub(super) fn terminate_pid(pid: u32) -> io::Result<()> {
    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    // SAFETY: the handle is valid and has PROCESS_TERMINATE access.
    if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The executable of the running process `pid`, if the platform tells us.
pub(super) fn executable_of(pid: u32) -> Option<PathBuf> {
    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut buf = vec![0u16; 32_768];
    let mut size = u32::try_from(buf.len()).ok()?;
    // SAFETY: the handle is valid, `buf` is writable for `size` UTF-16 units and
    // `size` points to writable memory.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw_handle(),
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if ok == 0 {
        return None;
    }
    buf.truncate(usize::try_from(size).ok()?);
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
}

/// Puts the suspended `child` in a new job and resumes it.
pub(super) fn attach_job(child: &tokio::process::Child, kill_on_close: bool) -> io::Result<Job> {
    let job = Job::new(kill_on_close)?;
    let process = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("the spawned zebrad has no process handle"))?;
    job.assign(process)?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("the spawned zebrad has no pid"))?;
    resume_threads(pid)?;
    Ok(job)
}

/// Resumes every thread of `pid`. A process created with `CREATE_SUSPENDED`
/// has exactly one, suspended once.
fn resume_threads(pid: u32) -> io::Result<()> {
    // SAFETY: a thread snapshot takes no pointers; the result is checked.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the snapshot handle was just created and is owned by nobody else.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).expect("small struct"),
        ..THREADENTRY32::default()
    };
    // SAFETY: `entry` is a properly sized THREADENTRY32 and the snapshot is valid.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    let mut resumed = 0usize;
    // The snapshot is finite; the walk ends with ERROR_NO_MORE_FILES.
    while found != 0 {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: OpenThread takes a thread id and returns a handle or null.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the thread handle was just opened and is owned by nobody else.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // SAFETY: the handle has THREAD_SUSPEND_RESUME access.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed += 1;
        }
        entry.dwSize = u32::try_from(size_of::<THREADENTRY32>()).expect("small struct");
        // SAFETY: same as Thread32First.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    if found == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != i32::try_from(ERROR_NO_MORE_FILES).ok() && resumed == 0 {
            return Err(err);
        }
    }
    if resumed == 0 {
        return Err(io::Error::other(format!(
            "no threads of the suspended zebrad (pid {pid}) were found to resume"
        )));
    }
    Ok(())
}
