//! Our sender against our receiver over mutual TLS on 127.0.0.1, on a random
//! port. No multicast and nothing on the network.

use nyx_share_localsend::identity::Identity;
use nyx_share_localsend::receiver::{Decision, End, Host, Offer, Receiver};
use nyx_share_localsend::sender::{self, Error, Outgoing, Source, Target};
use nyx_share_localsend::{DeviceInfo, PROTOCOL_VERSION};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

struct TestHost {
    me: DeviceInfo,
    folder: PathBuf,
    accept: bool,
    offers: Mutex<Vec<Offer>>,
    saved: Mutex<Vec<PathBuf>>,
    ended: Mutex<Vec<String>>,
}

impl Host for TestHost {
    fn me(&self) -> DeviceInfo {
        self.me.clone()
    }
    fn registered(&self, _: DeviceInfo, _: IpAddr, _: bool) {}
    fn offer(&self, offer: &Offer) -> Decision {
        self.offers.lock().unwrap().push(offer.clone());
        if self.accept { Decision::Accept } else { Decision::Decline }
    }
    fn refused(&self, _: &Offer, reason: &str) {
        panic!("refused: {reason}");
    }
    fn receive_folder(&self) -> PathBuf {
        self.folder.clone()
    }
    fn started(&self, _: &Offer) {}
    fn progress(&self, _: &str, _: u64) {}
    fn saved(&self, _: &str, path: PathBuf) {
        self.saved.lock().unwrap().push(path);
    }
    fn ended(&self, _: &str, end: End) {
        self.ended.lock().unwrap().push(match end {
            End::Completed => "completed".into(),
            End::Cancelled => "cancelled".into(),
            End::Failed(e) => e,
        });
    }
}

fn info(identity: &Identity, alias: &str) -> DeviceInfo {
    DeviceInfo {
        alias: alias.into(),
        version: PROTOCOL_VERSION.into(),
        device_model: Some("Linux".into()),
        device_type: Some("desktop".into()),
        fingerprint: identity.fingerprint.clone(),
        port: 0,
        protocol: "https".into(),
        download: false,
        announce: None,
    }
}

struct Pair {
    root: PathBuf,
    sender: Identity,
    host: Arc<TestHost>,
    receiver: Arc<Receiver>,
    receiver_fp: String,
}

fn pair(name: &str, accept: bool) -> Pair {
    let root = std::env::temp_dir().join(format!("nyx-share-loopback-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("inbox")).unwrap();
    let receiving = Identity::load_or_create(&root.join("a")).unwrap();
    let sender = Identity::load_or_create(&root.join("b")).unwrap();
    let host = Arc::new(TestHost {
        me: info(&receiving, "Receiver"),
        folder: root.join("inbox"),
        accept,
        offers: Mutex::default(),
        saved: Mutex::default(),
        ended: Mutex::default(),
    });
    let receiver = Receiver::start(&receiving, host.clone(), 0).unwrap();
    Pair { root, sender, host, receiver, receiver_fp: receiving.fingerprint.clone() }
}

impl Pair {
    fn target(&self, fingerprint: &str) -> Target {
        Target { fingerprint: fingerprint.into(), addr: IpAddr::V4(Ipv4Addr::LOCALHOST), port: self.receiver.port }
    }
    fn send(&self, fingerprint: &str, files: &[Outgoing<'_>]) -> Result<usize, Error> {
        sender::send(
            &self.sender,
            &info(&self.sender, "Sender"),
            &self.target(fingerprint),
            files,
            &AtomicBool::new(false),
            &|_| {},
        )
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn sends_files_that_the_receiver_accepts() {
    let p = pair("accept", true);
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(p.root.join("big.bin"), &big).unwrap();
    let file = std::fs::File::open(p.root.join("big.bin")).unwrap();
    let files = [
        Outgoing {
            name: "../escape.bin".into(),
            size: big.len() as u64,
            mime: "application/octet-stream".into(),
            preview: None,
            source: Source::File(&file),
        },
        Outgoing { name: "notes.txt".into(), size: 5, mime: "text/plain".into(), preview: None, source: Source::Bytes(b"hello") },
    ];
    assert_eq!(p.send(&p.receiver_fp, &files), Ok(2));
    let offer = p.host.offers.lock().unwrap()[0].clone();
    assert!(offer.verified, "the sender's client certificate matched its fingerprint");
    assert_eq!(offer.sender.alias, "Sender");
    assert_eq!(std::fs::read(p.root.join("inbox/escape.bin")).unwrap(), big);
    assert_eq!(std::fs::read(p.root.join("inbox/notes.txt")).unwrap(), b"hello");
    assert!(!p.root.join("escape.bin").exists());
    assert_eq!(p.host.ended.lock().unwrap().as_slice(), ["completed"]);
    assert!(!p.receiver.busy());
}

#[test]
fn reports_a_declined_offer() {
    let p = pair("decline", false);
    let files =
        [Outgoing { name: "a.txt".into(), size: 1, mime: "text/plain".into(), preview: None, source: Source::Bytes(b"a") }];
    assert_eq!(p.send(&p.receiver_fp, &files), Err(Error::Declined));
    assert!(std::fs::read_dir(p.root.join("inbox")).unwrap().next().is_none());
}

#[test]
fn text_messages_need_no_upload() {
    let p = pair("message", true);
    let files = [Outgoing {
        name: "m.txt".into(),
        size: 2,
        mime: "text/plain".into(),
        preview: Some("hi".into()),
        source: Source::Bytes(b"hi"),
    }];
    assert_eq!(p.send(&p.receiver_fp, &files), Ok(0));
    assert!(p.host.offers.lock().unwrap()[0].message_only);
}

#[test]
fn refuses_a_receiver_with_another_certificate() {
    let p = pair("pinning", true);
    let files = [Outgoing { name: "a".into(), size: 1, mime: "text/plain".into(), preview: None, source: Source::Bytes(b"a") }];
    let wrong = "00".repeat(32);
    match p.send(&wrong, &files) {
        Err(Error::Failed(m)) => assert!(m.contains("certificate"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(p.host.offers.lock().unwrap().is_empty(), "nothing reached the receiver");
}
