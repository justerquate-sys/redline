//! The shell's bridge to Android (JNI). Kotlin calls in (`app.lumen.tv.Native`: init, the
//! document-start script, the user agent, request rules, update accepted); Rust calls out to the
//! `app.lumen.tv.Host` object Kotlin handed over at init (Keystore sealing, bundle swap, script
//! change, update package installer, the speech recognizer). Off Android every call-out is a no-op (`seal` →
//! None: nothing is kept).

#[cfg(target_os = "android")]
mod imp {
    use jni::JNIEnv;
    use jni::JavaVM;
    use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
    use jni::sys::{jboolean, jint, jstring};
    use std::sync::OnceLock;

    static VM: OnceLock<JavaVM> = OnceLock::new();
    static HOST: OnceLock<GlobalRef> = OnceLock::new();

    fn with_host<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
        let vm = VM.get()?;
        let host = HOST.get()?;
        let mut env = vm.attach_current_thread().ok()?;
        let r = f(&mut env, host.as_obj());
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        r.ok()
    }

    fn bytes_call(name: &str, data: &[u8]) -> Option<Vec<u8>> {
        with_host(|env, h| {
            let arr = env.byte_array_from_slice(data)?;
            let r = env.call_method(h, name, "([B)[B", &[JValue::Object(&arr)])?.l()?;
            if r.is_null() {
                return Ok(None);
            }
            Ok(Some(env.convert_byte_array(JByteArray::from(r))?))
        })
        .flatten()
    }

    pub fn seal(data: &[u8]) -> Option<Vec<u8>> {
        bytes_call("seal", data)
    }

    pub fn open(data: &[u8]) -> Option<Vec<u8>> {
        bytes_call("open", data)
    }

    fn void_call(name: &str, arg: Option<&str>) {
        with_host(|env, h| {
            match arg {
                Some(a) => {
                    let s = env.new_string(a)?;
                    env.call_method(h, name, "(Ljava/lang/String;)V", &[JValue::Object(&s)])?;
                }
                None => {
                    env.call_method(h, name, "()V", &[])?;
                }
            }
            Ok(())
        });
    }

    pub fn bundle_ready() {
        void_call("bundleReady", None);
    }

    /// The page's script changed (telemetry on/off): installed for the next load before this returns.
    pub fn script_changed() {
        void_call("scriptChanged", None);
    }

    pub fn install_apk(path: &str) {
        void_call("installApk", Some(path));
    }

    fn bool_call(name: &str) -> bool {
        with_host(|env, h| env.call_method(h, name, "()Z", &[])?.z()).unwrap_or(false)
    }

    /// Android has a speech recognizer for us.
    pub fn voice_available() -> bool {
        bool_call("voiceAvailable")
    }

    /// Opens it (its answer comes back through `Native.voiceResult`); false when there is none.
    pub fn voice_start() -> bool {
        bool_call("voiceStart")
    }

    fn string(env: &mut JNIEnv, s: &JString) -> String {
        env.get_string(s).map(Into::into).unwrap_or_default()
    }

    fn jstr(env: &mut JNIEnv, s: &str) -> jstring {
        env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_init(mut env: JNIEnv, _c: JClass, dir: JString, host: JObject) {
        if let Ok(vm) = env.get_java_vm() {
            let _ = VM.set(vm);
        }
        if let Ok(g) = env.new_global_ref(host) {
            let _ = HOST.set(g);
        }
        let dir = string(&mut env, &dir);
        crate::shell::init(std::path::PathBuf::from(dir));
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_script(mut env: JNIEnv, _c: JClass) -> jstring {
        let s = crate::shell::get().map(|s| s.script()).unwrap_or_default();
        jstr(&mut env, &s)
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_userAgent(mut env: JNIEnv, _c: JClass, default: JString) -> jstring {
        let d = string(&mut env, &default);
        jstr(&mut env, &crate::script::user_agent(&d))
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_decide(mut env: JNIEnv, _c: JClass, url: JString, main_frame: jboolean, top: JString) -> jint {
        let url = string(&mut env, &url);
        let top = string(&mut env, &top);
        match crate::shell::get().map(|s| s.decide(&url, main_frame != 0, &top)) {
            Some(lumen_tv_core::rules::Decision::Block) => 1,
            Some(lumen_tv_core::rules::Decision::Cors) => 2,
            // A page the client renders: the shell's own document (lumen_tv_core::rules::CLIENT_DOCUMENT).
            Some(_) if main_frame != 0 && lumen_tv_core::rules::client_page(&url) => 3,
            // The client's Wasm (format 2): the bundle's, from here.
            Some(_) if lumen_tv_core::rules::client_wasm(&url) => 4,
            _ => 0,
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_wasm(env: JNIEnv, _c: JClass) -> jni::sys::jbyteArray {
        let b = crate::shell::get().map(|s| s.wasm()).unwrap_or_default();
        env.byte_array_from_slice(&b).map(|a| a.into_raw()).unwrap_or(std::ptr::null_mut())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_document(mut env: JNIEnv, _c: JClass) -> jstring {
        jstr(&mut env, lumen_tv_core::rules::CLIENT_DOCUMENT)
    }

    /// An intent for the page: validated in `lumen_tv_core::incoming`, kept for `shell_events`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_incoming(mut env: JNIEnv, _c: JClass, action: JString, data: JString, query: JString) {
        let (action, data, query) = (string(&mut env, &action), string(&mut env, &data), string(&mut env, &query));
        if let Some(s) = crate::shell::get() {
            s.incoming(&action, &data, &query);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_voiceResult(mut env: JNIEnv, _c: JClass, state: JString, text: JString) {
        let (state, text) = (string(&mut env, &state), string(&mut env, &text));
        if let Some(s) = crate::shell::get() {
            s.voice_result(&state, &text);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_lumen_tv_Native_acceptUpdate(_env: JNIEnv, _c: JClass) {
        if let Some(s) = crate::shell::get() {
            s.accept_update();
        }
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    pub fn seal(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn open(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn bundle_ready() {}
    pub fn script_changed() {}
    pub fn install_apk(_: &str) {}
    pub fn voice_available() -> bool {
        false
    }
    pub fn voice_start() -> bool {
        false
    }
}

pub use imp::{bundle_ready, install_apk, open, script_changed, seal, voice_available, voice_start};
