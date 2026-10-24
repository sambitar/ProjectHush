//! In-process backend for the phone build.
//!
//! The desktop is two processes: the Tauri shell and `hush-daemon`, joined by a
//! `0600` Unix socket. Android has no daemon process to hold that socket, so the
//! vault lives here and the same `/api/...` paths the UI already speaks are
//! dispatched in process (docs/MOBILE_APP_PLAN.md §5.1). Only the endpoints
//! routed below exist; everything else answers 501 rather than pretending a
//! desktop feature is present.
//!
//! The account lives on a dedicated thread with a current-thread runtime for the
//! same reason `AccountWorker` does: the Signal stores are not `Send`, so the
//! account is created, used and dropped on one thread.

use std::path::{Path, PathBuf};
use std::thread;

use base64::Engine as _;
use hush_core::{Account, InboxResult, MessagePayload, PlaceRole};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// Injected on every page load. The UI reads it before its first request, so
/// it must survive the WebView's own navigation to the app URL (an `eval`
/// issued during `setup` runs against the initial blank document and is lost).
pub fn init_js(stun_url: Option<&str>, turn: &hush_core::config::TurnConfig) -> String {
    let stun = serde_json::to_string(&stun_url).unwrap_or_else(|_| "null".into());
    let turn = serde_json::json!({
        "urls": &turn.urls,
        "username": &turn.username,
        "credential": &turn.credential,
    });
    format!(
        "window.__HUSH_API__ = null; \
         window.__HUSH_TRANSPORT__ = {{ kind: \"mobile\" }}; \
         window.__HUSH_STUN__ = {stun}; \
         window.__HUSH_TURN__ = {turn};"
    )
}

/// A daemon-shaped reply, so the UI can keep one request path on both targets.
#[derive(Debug, Serialize)]
pub struct ApiReply {
    pub status: u16,
    pub body: String,
}

fn ok_json(body: serde_json::Value) -> ApiReply {
    ApiReply {
        status: 200,
        body: body.to_string(),
    }
}

fn no_content() -> ApiReply {
    ApiReply {
        status: 204,
        body: String::new(),
    }
}

fn error(status: u16, message: &str) -> ApiReply {
    ApiReply {
        status,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

/// Work accepted by the backend thread. Every reply is a daemon-shaped one.
enum Work {
    Create {
        dir: PathBuf,
        passphrase: String,
        name: Option<String>,
        relay: Option<String>,
        reply: oneshot::Sender<ApiReply>,
    },
    Unlock {
        dir: PathBuf,
        passphrase: String,
        reply: oneshot::Sender<ApiReply>,
    },
    Lock {
        reply: oneshot::Sender<ApiReply>,
    },
    SetMailHandle {
        handle: String,
        reply: oneshot::Sender<ApiReply>,
    },
    Messages {
        reply: oneshot::Sender<ApiReply>,
    },
    Contacts {
        reply: oneshot::Sender<ApiReply>,
    },
    Mail {
        reply: oneshot::Sender<ApiReply>,
    },
    Conclaves {
        reply: oneshot::Sender<ApiReply>,
    },
    PendingInvites {
        reply: oneshot::Sender<ApiReply>,
    },
    ContactRequests {
        reply: oneshot::Sender<ApiReply>,
    },
    Invite {
        reply: oneshot::Sender<ApiReply>,
    },
    RequestContact {
        nickname: String,
        invite: String,
        note: Option<String>,
        reply: oneshot::Sender<ApiReply>,
    },
    AcceptContactRequest {
        account_id: String,
        nickname: Option<String>,
        reply: oneshot::Sender<ApiReply>,
    },
    DeclineContactRequest {
        account_id: String,
        reply: oneshot::Sender<ApiReply>,
    },
    CancelContactRequest {
        account_id: String,
        reply: oneshot::Sender<ApiReply>,
    },
    SendText {
        to: String,
        text: String,
        reply: oneshot::Sender<ApiReply>,
    },
    SendFileData {
        to: String,
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<ApiReply>,
    },
    CallOffer {
        to: String,
        session_id: String,
        sdp: String,
        reply: oneshot::Sender<ApiReply>,
    },
    CallAnswer {
        to: String,
        session_id: String,
        sdp: String,
        reply: oneshot::Sender<ApiReply>,
    },
    CallEnd {
        to: String,
        session_id: String,
        reason: String,
        reply: oneshot::Sender<ApiReply>,
    },
    Recv {
        reply: oneshot::Sender<ApiReply>,
    },
    Heartbeat {
        reply: oneshot::Sender<ApiReply>,
    },
    Places {
        reply: oneshot::Sender<ApiReply>,
    },
    AdoptPlace {
        nickname: String,
        invite: String,
        label: String,
        reply: oneshot::Sender<ApiReply>,
    },
    DetachPlace {
        account_id: String,
        reply: oneshot::Sender<ApiReply>,
    },
    IntroducePlace {
        place_account_id: String,
        contact_nickname: String,
        reply: oneshot::Sender<ApiReply>,
    },
    AcknowledgeRecovery {
        reply: oneshot::Sender<ApiReply>,
    },
    Status {
        dir: PathBuf,
        reply: oneshot::Sender<ApiReply>,
    },
}

/// Owns the account for the life of the app. Cloneable through Tauri state.
pub struct MobileBackend {
    dir: PathBuf,
    /// The STUN endpoint the WebView media path should use, from the vault's
    /// client config (with the shipped self-hosted default).
    stun_url: Option<String>,
    /// The TURN relay the WebView media path falls back to behind a symmetric
    /// NAT or client-isolating firewall.
    turn: hush_core::config::TurnConfig,
    tx: mpsc::Sender<Work>,
}

impl MobileBackend {
    /// Start the account thread. `dir` is the app-private vault directory.
    pub fn spawn(dir: PathBuf) -> Self {
        let stun_url = hush_core::config::resolve_stun_url(&dir);
        let turn = hush_core::config::resolve_turn_config(&dir);
        let (tx, mut rx) = mpsc::channel::<Work>(16);
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("mobile backend runtime");
            rt.block_on(async move {
                let mut account: Option<Account> = None;
                while let Some(work) = rx.recv().await {
                    let (reply, result) = match work {
                        Work::Create {
                            dir,
                            passphrase,
                            name,
                            relay,
                            reply,
                        } => {
                            // Creation deliberately leaves the vault locked: the
                            // caller holds the passphrase and unlocks next, so
                            // there is one unlock path rather than two.
                            (
                                reply,
                                create(&dir, &passphrase, name.as_deref(), relay.as_deref()).await,
                            )
                        }
                        Work::Unlock {
                            dir,
                            passphrase,
                            reply,
                        } => {
                            let result = match Account::open(&dir, &passphrase).await {
                                Ok(mut opened) => {
                                    // Refresh the prekey directory record the
                                    // pointer invite resolves through, exactly as
                                    // the desktop daemon does on unlock. Without
                                    // it this device keeps serving the record it
                                    // published at creation: its one-time pool
                                    // drains with every peer that accepts an
                                    // invite, and a record published by any other
                                    // writer for the same account no longer
                                    // matches these private halves. Best effort
                                    // with the same 20 s cap: a slow relay must
                                    // not hold the unlock reply.
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(20),
                                        opened.publish_prekeys(64),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            eprintln!("hush: prekey publish skipped: {e}")
                                        }
                                        Err(_) => {
                                            eprintln!("hush: prekey publish timed out")
                                        }
                                    }
                                    account = Some(opened);
                                    no_content()
                                }
                                Err(e) => error(401, &e.to_string()),
                            };
                            (reply, result)
                        }
                        Work::Lock { reply } => {
                            account = None;
                            (reply, no_content())
                        }
                        Work::SetMailHandle { handle, reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.set_mail_handle(&handle).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Messages { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(serde_json::json!(a.chat_history())),
                            };
                            (reply, result)
                        }
                        Work::Contacts { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(contacts_json(a).await),
                            };
                            (reply, result)
                        }
                        Work::Mail { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(serde_json::json!({
                                    "mail": a.mail_records(),
                                    "drafts": a.draft_records(),
                                    "drops": a.drop_records(),
                                })),
                            };
                            (reply, result)
                        }
                        Work::Conclaves { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(serde_json::json!(a.conclaves())),
                            };
                            (reply, result)
                        }
                        Work::PendingInvites { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(serde_json::json!(a.pending_invites())),
                            };
                            (reply, result)
                        }
                        Work::ContactRequests { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => {
                                    ok_json(serde_json::json!(a.incoming_contact_requests()))
                                }
                            };
                            (reply, result)
                        }
                        Work::Invite { reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.invite().await {
                                    Ok(invite) => ok_json(serde_json::json!({ "invite": invite })),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RequestContact {
                            nickname,
                            invite,
                            note,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => {
                                    match a
                                        .request_contact(&nickname, &invite, note.as_deref())
                                        .await
                                    {
                                        Ok(()) => no_content(),
                                        Err(e) => error(400, &e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::AcceptContactRequest {
                            account_id,
                            nickname,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => {
                                    match a
                                        .accept_contact_request(&account_id, nickname.as_deref())
                                        .await
                                    {
                                        Ok(nick) => {
                                            ok_json(serde_json::json!({ "nickname": nick }))
                                        }
                                        Err(e) => error(400, &e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::DeclineContactRequest { account_id, reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.decline_contact_request(&account_id).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::CancelContactRequest { account_id, reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.cancel_contact_request(&account_id).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendText { to, text, reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.send_text(&to, &text).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendFileData {
                            to,
                            name,
                            data,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.send_file_bytes(&to, &name, &data).await {
                                    Ok(id) => {
                                        ok_json(serde_json::json!({ "blob_id": hex::encode(id.0) }))
                                    }
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::CallOffer {
                            to,
                            session_id,
                            sdp,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.send_call_offer(&to, &session_id, &sdp).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::CallAnswer {
                            to,
                            session_id,
                            sdp,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.send_call_answer(&to, &session_id, &sdp).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::CallEnd {
                            to,
                            session_id,
                            reason,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.send_call_end(&to, &session_id, &reason).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Recv { reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.recv().await {
                                    Ok(inbox) => ok_json(recv_json(inbox)),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Heartbeat { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => match a.heartbeat_presence().await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Places { reply } => {
                            let result = match account.as_ref() {
                                None => error(400, "locked"),
                                Some(a) => ok_json(places_json(a)),
                            };
                            (reply, result)
                        }
                        Work::AdoptPlace {
                            nickname,
                            invite,
                            label,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.adopt_place(&nickname, &invite, &label).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DetachPlace { account_id, reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.detach_place(&account_id).await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::IntroducePlace {
                            place_account_id,
                            contact_nickname,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => {
                                    match a
                                        .introduce_place(&place_account_id, &contact_nickname)
                                        .await
                                    {
                                        Ok(()) => no_content(),
                                        Err(e) => error(400, &e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::AcknowledgeRecovery { reply } => {
                            let result = match account.as_mut() {
                                None => error(400, "locked"),
                                Some(a) => match a.acknowledge_recovery().await {
                                    Ok(()) => no_content(),
                                    Err(e) => error(400, &e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Status { dir, reply } => {
                            (reply, ok_json(status(&dir, account.as_ref())))
                        }
                    };
                    let _ = reply.send(result);
                }
            });
        });
        Self { dir, stun_url, turn, tx }
    }

    /// The STUN endpoint the WebView's peer connection should use.
    pub fn stun_url(&self) -> Option<&str> {
        self.stun_url.as_deref()
    }

    /// The TURN relay config the WebView's peer connection should use.
    pub fn turn_config(&self) -> &hush_core::config::TurnConfig {
        &self.turn
    }

    /// Dispatch one daemon-shaped request against the in-process account.
    ///
    /// `path` is matched exactly, without a query string, which is how the UI
    /// calls these four endpoints today.
    pub async fn dispatch(&self, method: &str, path: &str, body: Option<&str>) -> ApiReply {
        match (method, path) {
            ("GET", "/api/status") => {
                let dir = self.dir.clone();
                self.call(|reply| Work::Status { dir, reply }).await
            }
            ("POST", "/api/unlock") => match parse::<UnlockRequest>(body) {
                Ok(request) => {
                    let dir = self.dir.clone();
                    self.call(|reply| Work::Unlock {
                        dir,
                        passphrase: request.passphrase,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/vault/create") => match parse::<VaultCreateRequest>(body) {
                Ok(request) => {
                    let dir = self.dir.clone();
                    self.call(|reply| Work::Create {
                        dir,
                        passphrase: request.passphrase,
                        name: request.name,
                        relay: request.relay,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/lock") => self.call(|reply| Work::Lock { reply }).await,
            ("POST", "/api/mail/handle") => match parse::<SetMailHandleRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::SetMailHandle {
                        handle: request.handle,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("GET", "/api/messages") => self.call(|reply| Work::Messages { reply }).await,
            ("GET", "/api/contacts") => self.call(|reply| Work::Contacts { reply }).await,
            ("GET", "/api/mail") => self.call(|reply| Work::Mail { reply }).await,
            ("GET", "/api/conclaves") => self.call(|reply| Work::Conclaves { reply }).await,
            ("GET", "/api/conclaves/invites") => {
                self.call(|reply| Work::PendingInvites { reply }).await
            }
            ("GET", "/api/contact-requests") => {
                self.call(|reply| Work::ContactRequests { reply }).await
            }
            ("POST", "/api/invite") => self.call(|reply| Work::Invite { reply }).await,
            ("POST", "/api/contact-requests/request") => {
                match parse::<RequestContactRequest>(body) {
                    Ok(request) => {
                        self.call(|reply| Work::RequestContact {
                            nickname: request.nickname,
                            invite: request.invite,
                            note: request.note,
                            reply,
                        })
                        .await
                    }
                    Err(message) => error(400, &message),
                }
            }
            ("POST", "/api/contact-requests/accept") => match parse::<AcceptContactRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::AcceptContactRequest {
                        account_id: request.account_id,
                        nickname: request.nickname,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/contact-requests/decline") => {
                match parse::<ContactRequestTarget>(body) {
                    Ok(request) => {
                        self.call(|reply| Work::DeclineContactRequest {
                            account_id: request.account_id,
                            reply,
                        })
                        .await
                    }
                    Err(message) => error(400, &message),
                }
            }
            ("POST", "/api/contact-requests/cancel") => match parse::<ContactRequestTarget>(body) {
                Ok(request) => {
                    self.call(|reply| Work::CancelContactRequest {
                        account_id: request.account_id,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/send") => match parse::<SendTextRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::SendText {
                        to: request.to,
                        text: request.text,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/send-file-data") => match parse::<SendFileDataRequest>(body) {
                Ok(request) => {
                    match base64::engine::general_purpose::STANDARD
                        .decode(request.data_base64.trim())
                    {
                        Ok(data) => {
                            self.call(|reply| Work::SendFileData {
                                to: request.to,
                                name: request.name,
                                data,
                                reply,
                            })
                            .await
                        }
                        Err(_) => error(400, "invalid base64 file data"),
                    }
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/recv") => self.call(|reply| Work::Recv { reply }).await,
            ("POST", "/api/call/offer") => match parse::<CallSdpRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::CallOffer {
                        to: request.to,
                        session_id: request.session_id,
                        sdp: request.sdp,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/call/answer") => match parse::<CallSdpRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::CallAnswer {
                        to: request.to,
                        session_id: request.session_id,
                        sdp: request.sdp,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/call/end") => match parse::<CallEndRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::CallEnd {
                        to: request.to,
                        session_id: request.session_id,
                        reason: request.reason,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/presence/heartbeat") => {
                self.call(|reply| Work::Heartbeat { reply }).await
            }
            ("GET", "/api/places") => self.call(|reply| Work::Places { reply }).await,
            ("POST", "/api/places/adopt") => match parse::<AdoptPlaceRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::AdoptPlace {
                        nickname: request.nickname,
                        invite: request.invite,
                        label: request.label,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/places/detach") => match parse::<DetachPlaceRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::DetachPlace {
                        account_id: request.account_id,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/places/introduce") => match parse::<IntroducePlaceRequest>(body) {
                Ok(request) => {
                    self.call(|reply| Work::IntroducePlace {
                        place_account_id: request.place_account_id,
                        contact_nickname: request.contact_nickname,
                        reply,
                    })
                    .await
                }
                Err(message) => error(400, &message),
            },
            ("POST", "/api/recovery/acknowledge") => {
                self.call(|reply| Work::AcknowledgeRecovery { reply }).await
            }
            _ => error(
                501,
                "That screen is not available on the phone yet. The desktop app still has it.",
            ),
        }
    }

    async fn call(&self, make: impl FnOnce(oneshot::Sender<ApiReply>) -> Work) -> ApiReply {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(make(tx)).await.is_err() {
            return error(500, "The phone's vault backend stopped.");
        }
        rx.await
            .unwrap_or_else(|_| error(500, "The phone's vault backend stopped."))
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: Option<&str>) -> Result<T, String> {
    serde_json::from_str(body.unwrap_or_default()).map_err(|e| format!("bad request body: {e}"))
}

#[derive(Deserialize)]
struct UnlockRequest {
    passphrase: String,
}

#[derive(Deserialize)]
struct VaultCreateRequest {
    passphrase: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    relay: Option<String>,
}

#[derive(Deserialize)]
struct SetMailHandleRequest {
    handle: String,
}

#[derive(Deserialize)]
struct RequestContactRequest {
    nickname: String,
    invite: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct AcceptContactRequest {
    account_id: String,
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Deserialize)]
struct ContactRequestTarget {
    account_id: String,
}

#[derive(Deserialize)]
struct SendTextRequest {
    to: String,
    text: String,
}

#[derive(Deserialize)]
struct AdoptPlaceRequest {
    nickname: String,
    invite: String,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct DetachPlaceRequest {
    account_id: String,
}

#[derive(Deserialize)]
struct IntroducePlaceRequest {
    place_account_id: String,
    contact_nickname: String,
}

/// Settings → My places, shaped like the daemon's `PlaceDto`.
fn places_json(account: &Account) -> serde_json::Value {
    let list: Vec<serde_json::Value> = account
        .places()
        .iter()
        .map(|p| {
            serde_json::json!({
                "account_id": p.account_id,
                "nickname": p.nickname,
                "label": p.label,
                "role": match p.role {
                    PlaceRole::MintedByMe => "minted_by_me",
                    PlaceRole::MintedMe => "minted_me",
                },
                "confirmed": p.chain.verify_accepted().is_ok(),
                "added_unix": p.added_unix,
            })
        })
        .collect();
    serde_json::json!(list)
}

#[derive(Deserialize)]
struct SendFileDataRequest {
    to: String,
    name: String,
    data_base64: String,
}

#[derive(Deserialize)]
struct CallSdpRequest {
    to: String,
    session_id: String,
    sdp: String,
}

#[derive(Deserialize)]
struct CallEndRequest {
    to: String,
    session_id: String,
    #[serde(default = "default_hangup")]
    reason: String,
}

fn default_hangup() -> String {
    "hangup".to_string()
}

/// The poll response the UI reads, shaped like the daemon's `RecvResponse`.
///
/// Text, expiring lines and inline files are mapped. Mail and conclave payloads
/// are stored by `recv` but not drawn here; the phone has no conclave surface
/// and mail rendering is a separate slice.
fn recv_json(inbox: InboxResult) -> serde_json::Value {
    let InboxResult {
        messages,
        ephemeral,
        calls,
        signals,
        signal_acks,
        warnings,
        ..
    } = inbox;
    let mut rows = Vec::new();
    for message in messages {
        let from = message.from_nick;
        match message.payload {
            MessagePayload::Text { body } => rows.push(serde_json::json!({
                "from": from,
                "kind": "text",
                "text": body,
                "file_name": null,
                "file_size": null,
                "blob_id": null,
                "thread_id": null,
                "subject": null,
                "attachments": [],
                "conclave_id": null,
                "conclave_name": null,
                "conclave_trustees": [],
            })),
            MessagePayload::File { name, data } => rows.push(serde_json::json!({
                "from": from,
                "kind": "file",
                "text": null,
                "file_name": name,
                "file_size": data.len(),
                "blob_id": message.file_blob_id.map(|id| hex::encode(id.0)),
                "thread_id": null,
                "subject": null,
                "attachments": [],
                "conclave_id": null,
                "conclave_name": null,
                "conclave_trustees": [],
            })),
            _ => {}
        }
    }
    let calls: Vec<serde_json::Value> = calls
        .into_iter()
        .filter_map(|call| match call.payload {
            MessagePayload::CallOffer { session_id, sdp } => Some(serde_json::json!({
                "from": call.from_nick,
                "kind": "offer",
                "session_id": session_id,
                "sdp": sdp,
                "reason": null,
            })),
            MessagePayload::CallAnswer { session_id, sdp } => Some(serde_json::json!({
                "from": call.from_nick,
                "kind": "answer",
                "session_id": session_id,
                "sdp": sdp,
                "reason": null,
            })),
            MessagePayload::CallEnd {
                session_id, reason, ..
            } => Some(serde_json::json!({
                "from": call.from_nick,
                "kind": "end",
                "session_id": session_id,
                "sdp": null,
                "reason": reason,
            })),
            _ => None,
        })
        .collect();
    let expire_messages: Vec<serde_json::Value> = ephemeral
        .into_iter()
        .map(|line| {
            serde_json::json!({
                "from": line.from_nick,
                "id": line.id,
                "text": line.body,
                "sent_ms": line.sent_ms,
                "expires_ms": line.expires_ms,
            })
        })
        .collect();
    serde_json::json!({
        "messages": rows,
        "expire_messages": expire_messages,
        "calls": calls,
        "signals": signals,
        "signal_acks": signal_acks,
        "warnings": warnings,
        "chat_appended": false,
    })
}

/// The contacts the UI draws, shaped like the daemon's `ContactDto`.
///
/// Presence is a relay read per contact, the same one the daemon makes. A
/// failed read says nothing about the peer, so it falls back to offline rather
/// than failing the whole list.
async fn contacts_json(account: &Account) -> serde_json::Value {
    let mut list = Vec::new();
    for c in account.contacts() {
        let online = account
            .contact_is_online(&c.nickname)
            .await
            .unwrap_or(false);
        list.push(serde_json::json!({
            "nickname": c.nickname,
            "protocol_name": c.account_id,
            "inbound_ready": c.inbound_ready(),
            "online": online,
            "mail_handle": c.mail_handle,
            "mail_vault": c.mail_vault,
            "supports_file_transfer": c.supports_file_transfer,
            "reach": c.reach.narrow(c.peer_reach),
            "my_reach": c.reach,
            "place_of": c.place_of,
            "signals": c.signals,
            "payments": c.payments,
            "peer_payments": c.peer_payments,
            "torrents": c.torrents,
            "peer_torrents": c.peer_torrents,
            "wallet_bitcoin": c.wallet_bitcoin,
            "wallet_ethereum": c.wallet_ethereum,
            "wallet_updated_ms": c.wallet_updated_ms,
        }));
    }
    serde_json::json!(list)
}

/// Create this device's vault, mirroring the daemon's first-run rules.
///
/// Refusing an existing vault (409) is what stops a stale UI from overwriting an
/// identity. The relay is the request's, else the effective one for this
/// directory, and the plaintext copy in `client-config.json` is kept in step so
/// a later run knows which relay the vault was created against.
async fn create(dir: &Path, passphrase: &str, name: Option<&str>, relay: Option<&str>) -> ApiReply {
    if hush_core::vault_exists(dir) {
        return error(409, "This device already has a vault. Unlock it instead.");
    }
    if passphrase.is_empty() {
        return error(400, "A vault needs a passphrase.");
    }
    let relay = hush_core::config::resolve_relay_url(dir, relay);
    match Account::create(dir, passphrase, &relay).await {
        Ok(mut opened) => {
            let named = match name.map(str::trim).filter(|n| !n.is_empty()) {
                None => Ok(()),
                Some(n) => opened.set_vault_name(n).await,
            };
            if let Err(e) = named {
                return error(400, &e.to_string());
            }
            let mut cfg = hush_core::config::load(dir);
            cfg.relay_url = Some(opened.relay().to_string());
            if let Err(e) = hush_core::config::save(dir, &cfg) {
                eprintln!("hush: vault created but client-config.json was not written: {e}");
            }
            ok_json(serde_json::json!({
                "recovery_code": opened.take_pending_recovery_code(),
                "vault_name": opened.vault_name(),
                "relay": opened.relay(),
            }))
        }
        Err(e) => error(400, &e.to_string()),
    }
}

/// The daemon's `/api/status` shape, computed from the open account or from
/// disk while locked.
fn status(dir: &Path, account: Option<&Account>) -> serde_json::Value {
    let vault_present = hush_core::vault_exists(dir);
    match account {
        Some(a) => serde_json::json!({
            "locked": false,
            "vault_dir": a.vault_dir().display().to_string(),
            "protocol_name": a.protocol_name(),
            "vault_bytes": a.vault_bytes_on_disk(),
            "contacts": a.contacts().len(),
            "recovery_acknowledged": a.recovery_acknowledged(),
            "last_backup_unix": a.last_backup_unix(),
            "relay": a.relay(),
            "relay_source": "vault",
            "socket": null,
            "tcp_addr": null,
            "mail_handle": a.mail_handle(),
            "vault_name": a.vault_name(),
            "relay_tor": a.relay_tor_mode(),
            "vault_present": vault_present,
        }),
        None => {
            let (relay, source) = hush_core::config::resolve_relay_url_with_source(dir, None);
            serde_json::json!({
                "locked": true,
                "vault_dir": dir.display().to_string(),
                "protocol_name": null,
                "vault_bytes": 0,
                "contacts": 0,
                "recovery_acknowledged": true,
                "last_backup_unix": 0,
                "relay": relay,
                "relay_source": source.as_str(),
                "socket": null,
                "tcp_addr": null,
                "mail_handle": null,
                "vault_name": hush_core::DEFAULT_VAULT_NAME,
                "relay_tor": false,
                "vault_present": vault_present,
            })
        }
    }
}

/// Proxy one UI request to the in-process backend.
///
/// This is the phone's counterpart to the desktop `hush_api`, which proxies the
/// same paths to the daemon socket. Keeping the shape identical means the UI
/// needs one code path, not two.
#[cfg_attr(test, allow(dead_code))] // registered only by the mobile handler list
#[tauri::command]
pub async fn hush_mobile_api(
    state: tauri::State<'_, MobileBackend>,
    method: String,
    path: String,
    body: Option<String>,
) -> Result<ApiReply, String> {
    Ok(state.dispatch(&method, &path, body.as_deref()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(reply: &ApiReply) -> serde_json::Value {
        serde_json::from_str(&reply.body).expect("the backend replies with JSON")
    }

    #[tokio::test]
    async fn creates_unlocks_reports_and_locks() {
        let dir = tempfile::tempdir().expect("a temp vault dir");
        let backend = MobileBackend::spawn(dir.path().to_path_buf());

        let status = backend.dispatch("GET", "/api/status", None).await;
        assert_eq!(status.status, 200);
        let first = body(&status);
        assert_eq!(first["locked"], true);
        assert_eq!(first["vault_present"], false);

        let created = backend
            .dispatch(
                "POST",
                "/api/vault/create",
                Some(
                    r#"{"passphrase":"correct horse","name":"phone","relay":"http://127.0.0.1:1"}"#,
                ),
            )
            .await;
        assert_eq!(created.status, 200, "{}", created.body);
        let created = body(&created);
        assert!(created["recovery_code"].is_string());
        assert_eq!(created["vault_name"], "phone");

        // Creation deliberately leaves the vault locked.
        let status = backend.dispatch("GET", "/api/status", None).await;
        let after_create = body(&status);
        assert_eq!(after_create["locked"], true);
        assert_eq!(after_create["vault_present"], true);

        let wrong = backend
            .dispatch("POST", "/api/unlock", Some(r#"{"passphrase":"nope"}"#))
            .await;
        assert_eq!(wrong.status, 401);

        let unlocked = backend
            .dispatch(
                "POST",
                "/api/unlock",
                Some(r#"{"passphrase":"correct horse"}"#),
            )
            .await;
        assert_eq!(unlocked.status, 204, "{}", unlocked.body);

        let status = backend.dispatch("GET", "/api/status", None).await;
        let opened = body(&status);
        assert_eq!(opened["locked"], false);
        assert_eq!(opened["vault_name"], "phone");
        assert!(opened["protocol_name"].is_string());

        // The first-run flow completes the address while the vault is open.
        let handle = backend
            .dispatch("POST", "/api/mail/handle", Some(r#"{"handle":"sam"}"#))
            .await;
        assert_eq!(handle.status, 204, "{}", handle.body);
        let messages = backend.dispatch("GET", "/api/messages", None).await;
        assert_eq!(messages.status, 200);
        assert_eq!(body(&messages), serde_json::json!([]));
        // The unlock screen's refresh loads these; they must be real empty
        // results, not 501, or the address assignment after them is skipped.
        let contacts = backend.dispatch("GET", "/api/contacts", None).await;
        assert_eq!(contacts.status, 200);
        assert_eq!(body(&contacts), serde_json::json!([]));
        let mail = backend.dispatch("GET", "/api/mail", None).await;
        assert_eq!(mail.status, 200);
        assert_eq!(body(&mail)["mail"], serde_json::json!([]));
        let conclaves = backend.dispatch("GET", "/api/conclaves", None).await;
        assert_eq!(conclaves.status, 200);
        assert_eq!(body(&conclaves), serde_json::json!([]));
        // The contact and chat routes exist: a bad invite is a core 400, not
        // the phone's 501.
        let bad = backend
            .dispatch(
                "POST",
                "/api/contact-requests/request",
                Some(r#"{"nickname":"bo","invite":"hush1not-a-code"}"#),
            )
            .await;
        assert_eq!(bad.status, 400, "{}", bad.body);
        let send = backend
            .dispatch("POST", "/api/send", Some(r#"{"to":"bo","text":"hi"}"#))
            .await;
        assert_eq!(send.status, 400, "{}", send.body);
        let bad_file = backend
            .dispatch(
                "POST",
                "/api/send-file-data",
                Some(r#"{"to":"bo","name":"a.txt","data_base64":"!!!"}"#),
            )
            .await;
        assert_eq!(bad_file.status, 400, "{}", bad_file.body);
        let invite = backend.dispatch("POST", "/api/invite", None).await;
        assert_eq!(invite.status, 200, "{}", invite.body);
        assert!(body(&invite)["invite"]
            .as_str()
            .unwrap_or("")
            .starts_with("hush1"));
        let places = backend.dispatch("GET", "/api/places", None).await;
        assert_eq!(places.status, 200);
        assert_eq!(body(&places), serde_json::json!([]));
        let bad_place = backend
            .dispatch(
                "POST",
                "/api/places/adopt",
                Some(r#"{"nickname":"ph","invite":"hush1not-a-code","label":"phone"}"#),
            )
            .await;
        assert_eq!(bad_place.status, 400, "{}", bad_place.body);
        let status = backend.dispatch("GET", "/api/status", None).await;
        assert_eq!(body(&status)["mail_handle"], "sam");

        // First run shows the code once; acknowledging it closes the modal.
        let ack = backend
            .dispatch("POST", "/api/recovery/acknowledge", None)
            .await;
        assert_eq!(ack.status, 204, "{}", ack.body);
        let status = backend.dispatch("GET", "/api/status", None).await;
        assert_eq!(body(&status)["recovery_acknowledged"], true);

        let locked = backend.dispatch("POST", "/api/lock", None).await;
        assert_eq!(locked.status, 204);
        let status = backend.dispatch("GET", "/api/status", None).await;
        assert_eq!(body(&status)["locked"], true);

        // What the phone does not implement must fail loudly, not look empty.
        let missing = backend.dispatch("GET", "/api/notes/state", None).await;
        assert_eq!(missing.status, 501);
        assert!(missing.body.contains("not available on the phone"));
    }
}
