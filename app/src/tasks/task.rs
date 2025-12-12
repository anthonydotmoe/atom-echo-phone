use std::sync::{Arc, Barrier};

pub struct TaskMeta {
    pub name: &'static str,
    pub stack_bytes: Option<usize>,
}

pub trait AppTask {
    fn meta(&self) -> TaskMeta;

    /// Consume what you need from ctx, then
    /// return a closure that runs the task loop
    fn into_runner(self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static>;
}

pub trait Spawner {
    fn spawn(&self, meta: TaskMeta, f: Box<dyn FnOnce() + Send + 'static>);
}

pub fn start_all(tasks: Vec<Box<dyn AppTask>>) {
    let spawner = TaskSpawner;

    // +1 for the supervisor/main thread to release everybody
    let barrier = Arc::new(Barrier::new(tasks.len() + 1));

    // Build all runners first to heap allocate tasks before they run
    let mut runners: Vec<(TaskMeta, Box<dyn FnOnce() + Send>)> = Vec::with_capacity(tasks.len());
    for t in tasks {
        let meta = t.meta();
        let runner = t.into_runner();
        runners.push((meta, runner));
    }

    // Spawn them. Each will wait on the barrier
    for (meta, runner) in runners {
        let b = barrier.clone();
        spawner.spawn(meta, Box::new(move || {
            // Block on barrier
            b.wait();

            // Then run the task
            runner();
        }));
    }

    // Release them all at once.
    barrier.wait();
}

#[cfg(not(target_os = "espidf"))]
mod spawner {
    use super::{Spawner, TaskMeta};

    pub struct HostSpawner;

    impl Spawner for HostSpawner {
        fn spawn(&self, meta: TaskMeta, f: Box<dyn FnOnce() + Send + 'static>) {
            let mut b = std::thread::Builder::new().name(meta.name.into());
            if let Some(stack_sz) = meta.stack_bytes {
                b = b.stack_size(stack_sz);
            }

            b.spawn(move || f())
                .expect("spawn failed");
        }
    }
}
#[cfg(not(target_os = "espidf"))]
pub use spawner::HostSpawner as TaskSpawner;

#[cfg(target_os = "espidf")]
mod spawner {
    use esp_idf_svc::sys::{ESP_OK, esp_err_t, esp_pthread_cfg_t, esp_pthread_get_cfg, esp_pthread_get_default_config, esp_pthread_set_cfg};
    use std::ffi::{CString, c_char};

    use super::{Spawner, TaskMeta};

    pub struct EspSpawner;

    impl Spawner for EspSpawner {
        fn spawn(&self, meta: TaskMeta, f: Box<dyn FnOnce() + Send + 'static>) {
            let b = if let Some(stack_sz) = meta.stack_bytes {
                std::thread::Builder::new()
                    .stack_size(stack_sz)
            } else {
                std::thread::Builder::new()
            };

            let _ = with_next_pthread_cfg(meta, || b.spawn(f))
                .expect("spawn failed");
        }
    }

    fn with_next_pthread_cfg<T>(
        meta: TaskMeta,
        f: impl FnOnce() -> T
    ) -> Result<T, esp_err_t> {
        // FreeRTOS task name length is limited
        let cname = CString::new(meta.name).expect("no NULs in thread name");

        unsafe {
            // Save current per-thread config
            let mut prev: esp_pthread_cfg_t = core::mem::zeroed();
            let had_prev = esp_pthread_get_cfg(&mut prev) == ESP_OK;

            let mut cfg = if had_prev {
                prev
            } else {
                esp_pthread_get_default_config()
            };

            cfg.thread_name = cname.as_ptr() as *const c_char;

            if let Some(stack) = meta.stack_bytes {
                cfg.stack_size = stack;
            }

            let ret = esp_pthread_set_cfg(&cfg);
            if ret != ESP_OK {
                return Err(ret);
            }

            // Create the pthread while cfg is in effect
            let out = f();

            // Restore previous config for subsequent spawns from this thread.
            let restore = if had_prev { prev } else { esp_pthread_get_default_config() };
            let _ = esp_pthread_set_cfg(&restore);

            Ok(out)
        }
    }
}
#[cfg(target_os = "espidf")]
pub use spawner::EspSpawner as TaskSpawner;
