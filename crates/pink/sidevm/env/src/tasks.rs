use core::panic;
use std::task::Waker;

use super::*;

extern "Rust" {
    fn sidevm_main_future() -> Pin<Box<dyn Future<Output = ()>>>;
}

type TaskFuture = Pin<Box<dyn Future<Output = ()>>>;
/// When a task exited, the task future will be dropped, and it's space in this vector would be
/// set to None in order to reuse it's id in the future.
type Tasks = Vec<Option<TaskFuture>>;

thread_local! {
    /// The id of the current polling task. Would be passed to each ocall.
    static CURRENT_TASK: std::cell::Cell<i32>  = Default::default();
    /// All async tasks in the sidevm guest.
    static TASKS: RefCell<Tasks> = RefCell::new(vec![Some(unsafe { sidevm_main_future() })]);
    /// New spawned tasks are pushed to this queue. Since tasks are always spawned from inside a
    /// running task which borrowing the TASKS, it can not be immediately pushed to the TASKS.
    static SPAWNING_TASKS: RefCell<Vec<TaskFuture>> = RefCell::new(vec![]);
    /// New spawned tasks are pushed to this queue. Since tasks are always spawned from inside a
    /// running task which borrowing the TASKS, it can not be immediately pushed to the TASKS.
    static WAKERS: RefCell<Vec<Option<Waker>>> = RefCell::new(vec![]);
}

// TODO.kevin: Support task joining
pub struct TaskHandle;

pub fn intern_waker(waker: task::Waker) -> i32 {
    const MAX_N_WAKERS: usize = (i32::MAX / 2) as usize;
    WAKERS.with(|wakers| {
        let mut wakers = wakers.borrow_mut();
        for (id, waker_ref) in wakers.iter_mut().enumerate() {
            if waker_ref.is_none() {
                *waker_ref = Some(waker);
                return id as i32;
            }
        }
        if wakers.len() < MAX_N_WAKERS {
            wakers.push(Some(waker));
            wakers.len() as i32 - 1
        } else {
            panic!("Too many wakers");
        }
    })
}

fn wake_waker(waker_id: i32) {
    WAKERS.with(|wakers| {
        let wakers = wakers.borrow();
        if let Some(Some(waker)) = wakers.get(waker_id as usize) {
            waker.wake_by_ref();
        }
    });
}

fn drop_waker(waker_id: i32) {
    WAKERS.with(|wakers| {
        let mut wakers = wakers.borrow_mut();
        if let Some(waker) = wakers.get_mut(waker_id as usize) {
            *waker = None;
        }
    });
}

pub fn spawn(fut: impl Future<Output = ()> + 'static) -> TaskHandle {
    SPAWNING_TASKS.with(move |tasks| (*tasks).borrow_mut().push(Box::pin(fut)));
    TaskHandle
}

fn start_task(tasks: &mut Tasks, task: TaskFuture) {
    const MAX_N_TASKS: usize = (i32::MAX / 2) as _;

    for (task_id, task_ref) in tasks.iter_mut().enumerate().skip(1) {
        if task_ref.is_none() {
            *task_ref = Some(task);
            ocall::mark_task_ready(task_id as _).expect("Mark task ready failed");
            return;
        }
    }

    if tasks.len() < MAX_N_TASKS {
        let task_id = tasks.len();
        tasks.push(Some(task));
        ocall::mark_task_ready(task_id as _).expect("Mark task ready failed");
        return;
    }

    panic!("Spawn task failed, Max number of tasks reached");
}

fn start_spawned_tasks(tasks: &mut Tasks) {
    SPAWNING_TASKS.with(|spowned_tasks| {
        for task in spowned_tasks.borrow_mut().drain(..) {
            start_task(tasks, task);
        }
    })
}

pub(crate) fn current_task() -> i32 {
    CURRENT_TASK.with(|id| id.get())
}

fn set_current_task(task_id: i32) {
    CURRENT_TASK.with(|id| id.set(task_id))
}

fn poll_with_guest_context<F>(f: Pin<&mut F>) -> task::Poll<F::Output>
where
    F: Future + ?Sized,
{
    fn raw_waker(task_id: i32) -> task::RawWaker {
        task::RawWaker::new(
            task_id as _,
            &task::RawWakerVTable::new(
                |data| raw_waker(data as _),
                |data| {
                    let task_id = data as _;
                    ocall::mark_task_ready(task_id).expect("Mark task ready failed");
                },
                |data| {
                    let task_id = data as _;
                    ocall::mark_task_ready(task_id).expect("Mark task ready failed");
                },
                |_| (),
            ),
        )
    }
    let waker = unsafe { task::Waker::from_raw(raw_waker(current_task())) };
    let mut context = task::Context::from_waker(&waker);
    f.poll(&mut context)
}

#[no_mangle]
extern "C" fn sidevm_poll() -> i32 {
    use task::Poll::*;

    fn poll() -> task::Poll<()> {
        loop {
            for waker_id in ocall::awake_wakers().expect("Failed to get awaked wakers") {
                if waker_id >= 0 {
                    wake_waker(waker_id);
                } else {
                    drop_waker(-1 - waker_id);
                }
            }

            let task_id = match ocall::next_ready_task() {
                Ok(id) => id as usize,
                Err(OcallError::NotFound) => return task::Poll::Pending,
                Err(err) => panic!("Error occured: {:?}", err),
            };
            let exited = TASKS.with(|tasks| -> Option<bool> {
                let exited = {
                    let mut tasks = tasks.borrow_mut();
                    let task = tasks.get_mut(task_id)?.as_mut()?;
                    set_current_task(task_id as _);
                    match poll_with_guest_context(task.as_mut()) {
                        Pending => (),
                        Ready(()) => {
                            tasks[task_id] = None;
                        }
                    }
                    tasks[0].is_none()
                };
                if !exited {
                    start_spawned_tasks(&mut *tasks.borrow_mut());
                }
                Some(exited)
            });
            if let Some(true) = exited {
                return task::Poll::Ready(());
            }
        }
    }
    match poll() {
        Ready(()) => 1,
        Pending => 0,
    }
}
