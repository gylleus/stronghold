//! Notifications through `UNUserNotificationCenter`, for Myrstack running from `Myrstack.app`.
//!
//! The notification center needs a bundle identity: calling it from an unbundled binary throws,
//! so everything here is only used when [`is_bundled`]. [`install`] sets a delegate that shows
//! notifications even while Myrstack is frontmost and turns clicks into their payload's targets
//! on the [`clicks`] channel, and asks for permission to notify.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, Once, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use crossbeam_channel::{Receiver, Sender};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use super::{Outgoing, PAYLOAD_KEY, Poster, Repaint, decode_payload, encode_payload};

/// `CFBundleIdentifier` of `Myrstack.app`.
pub const BUNDLE_ID: &str = "ai.depict.myrstack";

/// Action identifier of a plain click on a notification.
const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";

/// Whether this process runs from `Myrstack.app` (its main bundle has Myrstack's identifier).
pub fn is_bundled() -> bool {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some_and(|id| id.to_string() == BUNDLE_ID)
}

/// How to ask for permission to notify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Ask the user (once; macOS remembers the answer).
    Ask,
    /// Provisional: no prompt, notifications arrive quietly in Notification Center until the
    /// user chooses to keep them. A later [`Permission::Ask`] still prompts.
    Quiet,
}

/// Sets the notification center's delegate and asks for permission. Idempotent.
pub fn install() {
    install_with(Permission::Ask);
}

/// [`install`] with a choice of [`Permission`]; only the first call in a process counts.
pub fn install_with(permission: Permission) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; it lives as long as the process.
        std::mem::forget(delegate);
        let done = RcBlock::new(|granted: Bool, error: *mut NSError| {
            // SAFETY: the error pointer is null or a valid NSError for this call.
            let answer = match unsafe { error.as_ref() } {
                _ if granted.as_bool() => Ok(()),
                Some(error) => Err(format!(
                    "notification permission request failed: {}",
                    error.localizedDescription()
                )),
                None => Err("notifications for Myrstack are turned off in System Settings".into()),
            };
            if let Err(reason) = &answer {
                log::warn!("{reason}");
            }
            authorization().set(answer);
        });
        let mut options = UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge;
        if permission == Permission::Quiet {
            options |= UNAuthorizationOptions::Provisional;
        }
        center.requestAuthorizationWithOptions_completionHandler(options, &done);
    });
}

/// Targets of clicked notifications; `wake` is called after each click.
pub fn clicks(wake: Option<Repaint>) -> Receiver<Vec<String>> {
    if let Ok(mut slot) = click_wake().lock() {
        *slot = wake;
    }
    click_channel().1.clone()
}

type ClickChannel = (Sender<Vec<String>>, Receiver<Vec<String>>);

fn click_channel() -> &'static ClickChannel {
    static CHANNEL: OnceLock<ClickChannel> = OnceLock::new();
    CHANNEL.get_or_init(crossbeam_channel::unbounded)
}

fn click_wake() -> &'static Mutex<Option<Repaint>> {
    static WAKE: Mutex<Option<Repaint>> = Mutex::new(None);
    &WAKE
}

fn clicked(targets: Vec<String>) {
    let _ = click_channel().0.send(targets);
    if let Ok(wake) = click_wake().lock()
        && let Some(wake) = wake.as_ref()
    {
        wake();
    }
}

/// The notification permission macOS has on record for Myrstack, for error messages.
fn authorization_status() -> &'static str {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let done = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the settings object is valid for the duration of the callback.
        let _ = tx.try_send(unsafe { settings.as_ref() }.authorizationStatus());
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&done);
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(UNAuthorizationStatus::NotDetermined) => "not asked yet",
        Ok(UNAuthorizationStatus::Denied) => {
            "denied; allow Myrstack in System Settings > Notifications"
        }
        Ok(UNAuthorizationStatus::Authorized) => "allowed",
        Ok(UNAuthorizationStatus::Provisional) => "provisional",
        Ok(_) => "other",
        Err(_) => "unknown",
    }
}

/// The answer to the permission request, once it arrives.
#[derive(Default)]
struct Authorization {
    answer: Mutex<Option<Result<(), String>>>,
    answered: Condvar,
}

impl Authorization {
    fn set(&self, answer: Result<(), String>) {
        if let Ok(mut slot) = self.answer.lock() {
            *slot = Some(answer);
        }
        self.answered.notify_all();
    }

    /// The answer, waiting up to `timeout` for it.
    fn wait(&self, timeout: Duration) -> Option<Result<(), String>> {
        let slot = self.answer.lock().ok()?;
        let (slot, _) = self
            .answered
            .wait_timeout_while(slot, timeout, |a| a.is_none())
            .ok()?;
        slot.clone()
    }
}

fn authorization() -> &'static Authorization {
    static AUTH: OnceLock<Authorization> = OnceLock::new();
    AUTH.get_or_init(Authorization::default)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Delegate` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "MyrstackNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            if response.actionIdentifier().to_string() == DEFAULT_ACTION {
                let user_info = response.notification().request().content().userInfo();
                let key = NSString::from_str(PAYLOAD_KEY);
                let payload = user_info
                    .objectForKey(&key)
                    .and_then(|v| v.downcast::<NSString>().ok())
                    .map(|s| s.to_string());
                match payload.as_deref().and_then(decode_payload) {
                    Some(targets) => clicked(targets),
                    None => log::debug!("clicked notification has no Myrstack payload"),
                }
            }
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// Posts through `UNUserNotificationCenter`; call [`install`] first.
#[derive(Debug)]
pub struct NativePoster {
    /// How long a post waits for the answer to the permission request.
    authorization_wait: Duration,
}

impl Default for NativePoster {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePoster {
    pub fn new() -> Self {
        Self {
            authorization_wait: Duration::from_secs(2),
        }
    }

    pub fn with_authorization_wait(mut self, wait: Duration) -> Self {
        self.authorization_wait = wait;
        self
    }
}

impl Poster for NativePoster {
    fn post(&self, notification: &Outgoing) -> anyhow::Result<()> {
        if let Some(Err(reason)) = authorization().wait(self.authorization_wait) {
            anyhow::bail!("{reason} (permission: {})", authorization_status());
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let key = NSString::from_str(PAYLOAD_KEY);
        let payload = NSString::from_str(&encode_payload(&notification.targets));
        let user_info = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*payload]);
        // SAFETY: the generic parameters are only a compile-time view of the same NSDictionary,
        // and a dictionary of strings is a valid property-list userInfo.
        unsafe { content.setUserInfo(&Retained::cast_unchecked::<NSDictionary>(user_info)) };
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&request_id()),
            &content,
            None,
        );
        let (tx, rx) = crossbeam_channel::bounded(1);
        let done = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: the error pointer is null or a valid NSError for this call.
            let error = unsafe { error.as_ref() }.map(|e| e.to_string());
            let _ = tx.try_send(error);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(None) => Ok(()),
            Ok(Some(error)) => anyhow::bail!("posting notification failed: {error}"),
            Err(_) => anyhow::bail!("posting notification timed out"),
        }
    }
}

/// A unique notification request identifier.
fn request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "myrstack-{}-{}-{}",
        std::process::id(),
        super::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
