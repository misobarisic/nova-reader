// Host boundary doubles leave the production JNI adapter unmodified.
mod paths {
    pub static ANDROID_DATA: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
}
mod reader_domain {
    pub struct DomainStore;
    impl DomainStore {
        pub fn open(_: std::path::PathBuf) -> Result<Self, String> {
            Ok(Self)
        }
    }
}
mod services {
    pub struct Services {
        pub domain: crate::reader_domain::DomainStore,
    }
    impl Services {
        pub fn open(_: &std::path::Path) -> Result<Self, String> {
            Ok(Self {
                domain: crate::reader_domain::DomainStore,
            })
        }
    }
}
mod sync_addons {
    pub struct Status {
        pub pending: usize,
        pub failed: usize,
        pub confirmation_required: usize,
    }
    pub fn reconcile(
        _: &crate::services::Services,
        _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Status, String> {
        Ok(Status {
            pending: 0,
            failed: 0,
            confirmation_required: 0,
        })
    }
}
mod sync {
    pub fn initialize(_: &crate::reader_domain::DomainStore) -> Result<(), String> {
        Ok(())
    }
    pub fn apply_remote(_: &crate::reader_domain::DomainStore) -> Result<(), String> {
        Ok(())
    }
}
mod nova_sync {
    pub struct Settings {
        pub enabled: bool,
        pub background_enabled: bool,
    }
    pub fn read_settings() -> Settings {
        Settings {
            enabled: false,
            background_enabled: false,
        }
    }
    pub fn notify_network_change() {}
    pub fn background_engine() -> Result<Option<Lease>, String> {
        Ok(None)
    }
    pub fn decode_ticket(_: &str) -> Result<(), String> {
        Err("not a ticket".into())
    }
    pub struct Lease {
        pub engine: Engine,
    }
    pub struct Engine;
    impl Engine {
        pub fn one_shot(&self, _: &std::sync::atomic::AtomicBool, _: std::time::Duration) {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_bounds_and_cancellation() {
        let args = jni::InitArgsBuilder::new()
            .version(jni::JNIVersion::V8)
            .build()
            .unwrap();
        let vm = jni::JavaVM::new(args).unwrap();
        let env = vm.attach_current_thread().unwrap();
        for (width, height, stride, bytes) in [
            (0, 4, 4, vec![0; 16]),
            (4, -1, 4, vec![0; 16]),
            (4, 4, 3, vec![0; 16]),
            (4, 4, 4, vec![0; 15]),
            (i32::MAX, i32::MAX, i32::MAX, vec![]),
            (4, 4, 4, vec![255; 16]),
        ] {
            let frame = env.byte_array_from_slice(&bytes).unwrap();
            let result = Java_dev_misob_nova_reader_QrScanActivity_nativeOnFrame(
                unsafe { env.unsafe_clone() },
                JObject::null(),
                frame,
                width,
                height,
                stride,
            );
            assert_eq!(result, JNI_FALSE);
        }
        assert_eq!(
            Java_dev_misob_nova_reader_QrScanActivity_nativeHasRequest(
                unsafe { env.unsafe_clone() },
                JObject::null()
            ),
            JNI_FALSE
        );
        let (tx, rx) = std::sync::mpsc::channel();
        *SCAN.lock().unwrap() = Some(Box::new(move |result| tx.send(result).unwrap()));
        assert_eq!(
            Java_dev_misob_nova_reader_QrScanActivity_nativeHasRequest(
                unsafe { env.unsafe_clone() },
                JObject::null()
            ),
            JNI_TRUE
        );
        Java_dev_misob_nova_reader_QrScanActivity_nativeCancelled(
            unsafe { env.unsafe_clone() },
            JObject::null(),
        );
        assert!(rx.recv().unwrap().is_err());
        assert!(SCAN.lock().unwrap().is_none());
        Java_dev_misob_nova_reader_ReaderSyncJobService_nativePrepareSync(
            unsafe { env.unsafe_clone() },
            JObject::null(),
        );
        assert!(!job_cancel().load(Ordering::Acquire));
        let previous = job_cancel();
        Java_dev_misob_nova_reader_ReaderSyncJobService_nativePrepareSync(
            unsafe { env.unsafe_clone() },
            JObject::null(),
        );
        previous.store(true, Ordering::Release);
        assert!(
            !job_cancel().load(Ordering::Acquire),
            "old deadlines cannot cancel a new job"
        );
        Java_dev_misob_nova_reader_ReaderSyncJobService_nativeCancelSync(
            unsafe { env.unsafe_clone() },
            JObject::null(),
        );
        assert!(job_cancel().load(Ordering::Acquire));
    }
}
