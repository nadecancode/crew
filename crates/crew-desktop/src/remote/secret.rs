//! Linux OS credential adapter for gateway device keys.
//!
//! Each remote profile's Ed25519 signing seed is one item in the user's
//! Secret Service default collection (`org.freedesktop.secrets`, provided by
//! GNOME Keyring, KWallet and compatible services). The connector supplies
//! typed transcripts; this adapter returns signatures and public keys only.
//! Seeds never reach profiles, view records, diagnostics or logs, and there is
//! no plaintext-file fallback. Without a usable service, the only alternative
//! is the transport's explicit memory-only signer.
//!
//! The session uses the Secret Service `plain` algorithm: the per-user session
//! bus carries the seed to this process, as it does for libsecret clients that
//! cannot negotiate transport encryption. The seed is copied into zeroizing
//! buffers; GLib's transient message buffers are outside this adapter's control.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crew_client_transport::gateway::{
    AuthTranscript, DeviceSigner, EnrollTranscript, SignerError, SignerFuture,
};
use crew_protocol::native::{HexBytes, PublicKey, Signature};
use ed25519_dalek::Signer as _;
use gtk::gio;
use gtk::glib::{
    self,
    variant::{ObjectPath, ToVariant},
    Variant, VariantTy,
};
use zeroize::Zeroizing;

const BUS_NAME: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const SESSION: &str = "org.freedesktop.Secret.Session";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// Lookup attribute naming this application's device-key items.
pub const SCHEMA: &str = "dev.crew.Crew.DeviceKey";
/// Signing completes well inside the connector's 5 s proof deadline.
const SIGN_CALL_MS: i32 = 2_000;
const CALL_MS: i32 = 10_000;
/// User presence for unlock stays inside the connector's 30 s unlock bound.
const PROMPT_LIMIT: Duration = Duration::from_secs(25);

/// Secure-storage readiness shown before a device key is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreState {
    Ready,
    Locked,
    Unavailable,
}

/// Whether preparing a profile created its key or found the existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOrigin {
    Existing,
    Created,
}

fn ty(text: &'static str) -> &'static VariantTy {
    VariantTy::new(text).expect("static D-Bus reply type")
}

fn classify(error: glib::Error) -> SignerError {
    match gio::DBusError::remote_error(&error).as_deref() {
        Some("org.freedesktop.Secret.Error.IsLocked") => SignerError::Locked,
        _ => SignerError::Unavailable,
    }
}

/// One Secret Service session. Dropping it closes the session.
struct Store {
    bus: gio::DBusConnection,
    session: ObjectPath,
}

impl Store {
    fn open() -> Result<Self, SignerError> {
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .map_err(|_| SignerError::Unavailable)?;
        let reply = call(
            &bus,
            SERVICE_PATH,
            SERVICE,
            "OpenSession",
            Some(&("plain", "".to_variant()).to_variant()),
            "(vo)",
            CALL_MS,
        )?;
        let session = reply
            .child_value(1)
            .get::<ObjectPath>()
            .ok_or(SignerError::Unavailable)?;
        Ok(Self { bus, session })
    }

    fn call(
        &self,
        object: &str,
        interface: &str,
        method: &str,
        args: Option<&Variant>,
        reply: &'static str,
        timeout: i32,
    ) -> Result<Variant, SignerError> {
        call(&self.bus, object, interface, method, args, reply, timeout)
    }

    fn default_collection(&self) -> Result<ObjectPath, SignerError> {
        let reply = self.call(
            SERVICE_PATH,
            SERVICE,
            "ReadAlias",
            Some(&("default",).to_variant()),
            "(o)",
            CALL_MS,
        )?;
        let collection = reply
            .child_value(0)
            .get::<ObjectPath>()
            .ok_or(SignerError::Unavailable)?;
        if collection.as_str() == "/" {
            return Err(SignerError::Unavailable);
        }
        Ok(collection)
    }

    fn locked(&self, object: &ObjectPath, interface: &str) -> Result<bool, SignerError> {
        let reply = self.call(
            object.as_str(),
            PROPERTIES,
            "Get",
            Some(&(interface, "Locked").to_variant()),
            "(v)",
            CALL_MS,
        )?;
        reply
            .child_value(0)
            .as_variant()
            .and_then(|value| value.get::<bool>())
            .ok_or(SignerError::Unavailable)
    }

    fn search(
        &self,
        attributes: &HashMap<String, String>,
        timeout: i32,
    ) -> Result<(Vec<ObjectPath>, Vec<ObjectPath>), SignerError> {
        let reply = self.call(
            SERVICE_PATH,
            SERVICE,
            "SearchItems",
            Some(&(attributes.clone(),).to_variant()),
            "(aoao)",
            timeout,
        )?;
        let unlocked = reply.child_value(0).get::<Vec<ObjectPath>>();
        let locked = reply.child_value(1).get::<Vec<ObjectPath>>();
        unlocked.zip(locked).ok_or(SignerError::Unavailable)
    }

    /// Asks the service to unlock, showing its own prompt if needed. Only
    /// public-key retrieval and explicit user actions call this.
    fn unlock(&self, objects: Vec<ObjectPath>) -> Result<(), SignerError> {
        let reply = self.call(
            SERVICE_PATH,
            SERVICE,
            "Unlock",
            Some(&(objects,).to_variant()),
            "(aoo)",
            CALL_MS,
        )?;
        let prompt = reply
            .child_value(1)
            .get::<ObjectPath>()
            .ok_or(SignerError::Unavailable)?;
        self.prompt(&prompt)
    }

    fn prompt(&self, prompt: &ObjectPath) -> Result<(), SignerError> {
        if prompt.as_str() == "/" {
            return Ok(());
        }
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let done = Rc::new(Cell::new(None::<bool>));
                let _subscription = self.bus.subscribe_to_signal(
                    Some(BUS_NAME),
                    Some(PROMPT),
                    Some("Completed"),
                    Some(prompt.as_str()),
                    None,
                    gio::DBusSignalFlags::NONE,
                    {
                        let done = done.clone();
                        move |signal| {
                            let dismissed = signal.parameters.child_value(0).get::<bool>();
                            done.set(Some(dismissed.unwrap_or(true)));
                        }
                    },
                );
                self.call(
                    prompt.as_str(),
                    PROMPT,
                    "Prompt",
                    Some(&("",).to_variant()),
                    "()",
                    CALL_MS,
                )?;
                let deadline = Instant::now() + PROMPT_LIMIT;
                while done.get().is_none() && Instant::now() < deadline {
                    if !context.iteration(false) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                match done.get() {
                    Some(false) => Ok(()),
                    Some(true) => Err(SignerError::Canceled),
                    None => {
                        let _ = self.call(prompt.as_str(), PROMPT, "Dismiss", None, "()", CALL_MS);
                        Err(SignerError::Canceled)
                    }
                }
            })
            .map_err(|_| SignerError::Unavailable)?
    }

    fn secret(&self, item: &ObjectPath, timeout: i32) -> Result<Zeroizing<[u8; 32]>, SignerError> {
        let reply = self.call(
            item.as_str(),
            ITEM,
            "GetSecret",
            Some(&(self.session.clone(),).to_variant()),
            "((oayays))",
            timeout,
        )?;
        let value = reply.child_value(0).child_value(2);
        let bytes = value
            .fixed_array::<u8>()
            .map_err(|_| SignerError::Unavailable)?;
        let mut seed = Zeroizing::new([0; 32]);
        if bytes.len() != seed.len() {
            return Err(SignerError::InvalidProof);
        }
        seed.copy_from_slice(bytes);
        Ok(seed)
    }

    fn create(
        &self,
        collection: &ObjectPath,
        label: &str,
        attributes: &HashMap<String, String>,
        seed: &[u8; 32],
    ) -> Result<(), SignerError> {
        let mut properties = HashMap::<String, Variant>::new();
        properties.insert(
            "org.freedesktop.Secret.Item.Label".into(),
            label.to_variant(),
        );
        properties.insert(
            "org.freedesktop.Secret.Item.Attributes".into(),
            attributes.to_variant(),
        );
        let secret = Variant::tuple_from_iter([
            self.session.to_variant(),
            Variant::array_from_fixed_array::<u8>(&[]),
            Variant::array_from_fixed_array(&seed[..]),
            "application/octet-stream".to_variant(),
        ]);
        let reply = self.call(
            collection.as_str(),
            COLLECTION,
            "CreateItem",
            Some(&Variant::tuple_from_iter([
                properties.to_variant(),
                secret,
                false.to_variant(),
            ])),
            "(oo)",
            CALL_MS,
        )?;
        let prompt = reply
            .child_value(1)
            .get::<ObjectPath>()
            .ok_or(SignerError::Unavailable)?;
        self.prompt(&prompt)
    }

    fn delete(&self, item: &ObjectPath) -> Result<(), SignerError> {
        let reply = self.call(item.as_str(), ITEM, "Delete", None, "(o)", CALL_MS)?;
        let prompt = reply
            .child_value(0)
            .get::<ObjectPath>()
            .ok_or(SignerError::Unavailable)?;
        self.prompt(&prompt)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.call(self.session.as_str(), SESSION, "Close", None, "()", 1_000);
    }
}

fn call(
    bus: &gio::DBusConnection,
    object: &str,
    interface: &str,
    method: &str,
    args: Option<&Variant>,
    reply: &'static str,
    timeout: i32,
) -> Result<Variant, SignerError> {
    bus.call_sync(
        Some(BUS_NAME),
        object,
        interface,
        method,
        args,
        Some(ty(reply)),
        gio::DBusCallFlags::NONE,
        timeout,
        gio::Cancellable::NONE,
    )
    .map_err(classify)
}

/// Blocking readiness check for the device-name page. Runs on a worker.
pub fn probe() -> StoreState {
    let state = Store::open().and_then(|store| {
        let collection = store.default_collection()?;
        store.locked(&collection, COLLECTION)
    });
    match state {
        Ok(false) => StoreState::Ready,
        Ok(true) | Err(SignerError::Locked) => StoreState::Locked,
        Err(_) => StoreState::Unavailable,
    }
}

/// A profile's device key in the Secret Service.
#[derive(Clone)]
pub struct SecretServiceSigner {
    attributes: HashMap<String, String>,
    label: String,
}

impl std::fmt::Debug for SecretServiceSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretServiceSigner(<OS credential service>)")
    }
}

impl SecretServiceSigner {
    pub fn new(profile_id: &str, endpoint_id: &str, profile_label: &str) -> Self {
        Self {
            attributes: HashMap::from([
                ("xdg:schema".to_owned(), SCHEMA.to_owned()),
                ("profile".to_owned(), profile_id.to_owned()),
                ("endpoint".to_owned(), endpoint_id.to_owned()),
            ]),
            label: format!("Crew device key · {profile_label}"),
        }
    }

    /// Blocking. Uses this profile's existing key, or generates one from OS
    /// entropy and stores it. Runs only after an explicit user choice, so the
    /// service may show its unlock prompt.
    pub fn prepare(&self) -> Result<KeyOrigin, SignerError> {
        let store = Store::open()?;
        let (unlocked, locked) = store.search(&self.attributes, CALL_MS)?;
        if !unlocked.is_empty() || !locked.is_empty() {
            if unlocked.is_empty() {
                store.unlock(locked)?;
            }
            return Ok(KeyOrigin::Existing);
        }
        let collection = store.default_collection()?;
        if store.locked(&collection, COLLECTION)? {
            store.unlock(vec![collection.clone()])?;
        }
        let mut seed = Zeroizing::new([0; 32]);
        getrandom::fill(&mut seed[..]).map_err(|_| SignerError::Unavailable)?;
        store.create(&collection, &self.label, &self.attributes, &seed)?;
        Ok(KeyOrigin::Created)
    }

    /// Blocking. Never creates a key; prompts only when `may_prompt`.
    fn seed(&self, may_prompt: bool, timeout: i32) -> Result<Zeroizing<[u8; 32]>, SignerError> {
        let store = Store::open()?;
        let (unlocked, locked) = store.search(&self.attributes, timeout)?;
        let item = match (unlocked.first(), locked.first()) {
            (Some(item), _) => item.clone(),
            (None, Some(item)) if may_prompt => {
                store.unlock(vec![item.clone()])?;
                item.clone()
            }
            (None, Some(_)) => return Err(SignerError::Locked),
            (None, None) => return Err(SignerError::Retired),
        };
        store.secret(&item, timeout)
    }

    fn sign(&self, bytes: Vec<u8>) -> SignerFuture<'static, Signature> {
        let signer = self.clone();
        off_thread(move || {
            let seed = signer.seed(false, SIGN_CALL_MS)?;
            let key = ed25519_dalek::SigningKey::from_bytes(&seed);
            Ok(HexBytes(key.sign(&bytes).to_bytes()))
        })
    }

    /// Blocking. Deletes every item for this profile; the host grant remains.
    fn delete(&self) -> Result<(), SignerError> {
        let store = Store::open()?;
        let (unlocked, locked) = store.search(&self.attributes, CALL_MS)?;
        if !locked.is_empty() {
            store.unlock(locked.clone())?;
        }
        for item in unlocked.iter().chain(&locked) {
            store.delete(item)?;
        }
        Ok(())
    }
}

/// D-Bus calls block, so each runs on its own short-lived thread. Dropping
/// the returned future abandons the wait; the bounded call ends on its own.
fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, SignerError> + Send + 'static,
) -> SignerFuture<'static, T> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("crew-secret-service".into())
        .spawn(move || {
            let _ = sender.send(work());
        });
    Box::pin(async move {
        spawned.map_err(|_| SignerError::Unavailable)?;
        receiver.await.map_err(|_| SignerError::Unavailable)?
    })
}

impl DeviceSigner for SecretServiceSigner {
    fn public_key(&self) -> SignerFuture<'_, PublicKey> {
        let signer = self.clone();
        off_thread(move || {
            let seed = signer.seed(true, CALL_MS)?;
            Ok(HexBytes(
                ed25519_dalek::SigningKey::from_bytes(&seed)
                    .verifying_key()
                    .to_bytes(),
            ))
        })
    }
    fn sign_auth(&self, transcript: AuthTranscript) -> SignerFuture<'_, Signature> {
        self.sign(transcript.signing_bytes().to_vec())
    }
    fn sign_enrollment(&self, transcript: EnrollTranscript) -> SignerFuture<'_, Signature> {
        self.sign(transcript.signing_bytes().to_vec())
    }
    fn retire(&self) -> SignerFuture<'_, ()> {
        let signer = self.clone();
        off_thread(move || signer.delete())
    }
}
