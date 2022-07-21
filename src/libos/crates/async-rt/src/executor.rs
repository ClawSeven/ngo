use futures::task::waker_ref;

use crate::load_balancer::LoadBalancer;
use crate::prelude::*;
#[allow(unused_imports)]
use crate::task::Task;
use crate::vcpu;

use crate::scheduler::Scheduler;

pub fn num_vcpus() -> u32 {
    EXECUTOR.num_vcpus()
}

// Returning number of running vcpus
pub fn run_tasks() -> u32 {
    EXECUTOR.run_tasks()
}

pub fn shutdown() {
    EXECUTOR.shutdown()
}

lazy_static! {
    pub(crate) static ref EXECUTOR: Executor = {
        let num_vcpus = vcpu::get_total();
        Executor::new(num_vcpus).unwrap()
    };
}

pub(crate) struct Executor {
    num_vcpus: u32,
    running_vcpus: AtomicU32,
    scheduler: Arc<Scheduler<Task>>,
    is_shutdown: AtomicBool,
    load_balancer: LoadBalancer,
}

impl Executor {
    pub fn new(num_vcpus: u32) -> Result<Self> {
        if num_vcpus == 0 {
            return_errno!(EINVAL, "invalid argument");
        }

        let running_vcpus = AtomicU32::new(0);
        let scheduler = Arc::new(Scheduler::new(num_vcpus));
        let is_shutdown = AtomicBool::new(false);
        let load_balancer = LoadBalancer::new(scheduler.clone());

        let new_self = Self {
            num_vcpus,
            running_vcpus,
            scheduler,
            is_shutdown,
            load_balancer,
        };
        Ok(new_self)
    }

    pub fn num_vcpus(&self) -> u32 {
        self.num_vcpus
    }

    pub fn run_tasks(&self) -> u32 {
        let this_vcpu = self.running_vcpus.fetch_add(1, Ordering::Relaxed);
        debug_assert!(this_vcpu < self.num_vcpus);

        vcpu::set_current(this_vcpu);

        let local_schedulers = self.scheduler.local_schedulers();
        let parker = local_schedulers[this_vcpu as usize].parker();
        parker.register();

        // Todo: need retries
        'outer: loop {
            let mut task = None;
            // Dequeue task on this vcpu, if None, wait for enqueueing task
            'inner: loop {
                if self.is_shutdown() {
                    break 'outer;
                }

                if let Some(entity) = self.scheduler.dequeue(this_vcpu) {
                    task = Some(entity);
                    break 'inner;
                }

                self.scheduler.wait_enqueue(this_vcpu);
            }

            debug_assert!(task.is_some());
            let task = task.unwrap();

            let mut future_slot = match task.future().try_lock() {
                Some(future) => future,
                None => {
                    // The task happens to be executed by other vCPUs at the moment.
                    // Try to execute it later.
                    debug!("the task happens to be executed by other vcpus, re-enqueue it");
                    self.scheduler.enqueue(&task, Some(this_vcpu));
                    continue;
                }
            };

            let future = match future_slot.as_mut() {
                Some(future) => future,
                None => {
                    debug!("the task happens to be completed, task: {:?}", task.tid());
                    // The task happens to be completed
                    continue;
                }
            };

            crate::task::current::set(task.clone());

            // Obtain task timeslice, set the timer of corresponding timeslice for task running
            // let timeslice :u32 = task.sched_state().timeslice();
            // let mut timeout = Duration::millisec(timeslice);
            // let timer_entry = TimerEntry::new(timeout);
            // let timer_future = TimerFutureEntry::new(&timer_entry);

            // Firstly poll timer future, insert timer

            // Execute task
            let waker = waker_ref(&task);
            let context = &mut Context::from_waker(&*waker);

            debug!("Poll the task");
            let ret = future.as_mut().poll(context);

            if let Poll::Ready(()) = ret {
                // As the task is completed, we can destory the future
                drop(future_slot.take());
            }

            // Secondly poll timer future, obtain time

            // Reset current task
            crate::task::current::reset();
        }

        parker.unregister();
        vcpu::clear_current();
        // todo: remove this part into shutdown
        let num = self.running_vcpus.load(Ordering::Relaxed);
        num - 1
    }

    // Accept a new task and schedule it, mainly called by spawn
    pub fn accept_task(&self, task: Arc<Task>) {
        if self.is_shutdown() {
            panic!("a shut-down executor cannot spawn new tasks");
        }

        self.schedule_task(&task);
    }

    /// Wake up an old task and schedule it, mainly called by wake_by_ref
    pub fn wake_task(&self, task: &Arc<Task>) {
        if self.is_shutdown() {
            // TODO: What to do if there are still task in the run queues
            // of the scheduler when the executor is shutdown.
            // e.g., yield-loop tasks might be waken up when the executer
            // is shutdown.
            debug!("task {:?} is running when executor shut-down", task.tid());
            return;
        }

        self.schedule_task(task);
    }

    pub fn schedule_task(&self, task: &Arc<Task>) {
        debug!("enqueue task");
        self.scheduler.enqueue(task, vcpu::get_current());
    }

    pub fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::Relaxed)
    }

    pub fn shutdown(&self) {
        self.stop_load_balancer();
        self.is_shutdown.store(true, Ordering::Relaxed);

        self.scheduler.wake_all();
        crate::time::wake_timer_wheel(&Duration::default()); // wake the time wheel right now
    }

    pub fn start_load_balancer(&self) {
        self.load_balancer.start();
    }

    pub fn stop_load_balancer(&self) {
        self.load_balancer.stop();
    }
}
