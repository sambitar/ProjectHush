use std::path::PathBuf;
use std::thread;

use hush_core::{Account, ChatRecord, InboxResult};
use tokio::sync::{mpsc, oneshot};

pub struct AccountWorker {
    tx: mpsc::Sender<Work>,
}

/// The one credential a drop is sealed under.
///
/// An enum rather than two `Option`s so a caller cannot pass both, and so the
/// worker has to say which mode it means. The HTTP layer builds this after
/// rejecting a request that gave both or neither.
pub enum DropSecret {
    /// A PIN the recipient is told out of band.
    Pin(String),
    /// The recipient's pasteable drop public key.
    PublicKey(String),
}

enum Work {
    Unlock {
        dir: PathBuf,
        passphrase: String,
        reply: oneshot::Sender<WorkResult>,
    },
    Lock {
        reply: oneshot::Sender<WorkResult>,
    },
    Invite {
        reply: oneshot::Sender<WorkResult>,
    },
    Contacts {
        reply: oneshot::Sender<WorkResult>,
    },
    AddContact {
        nickname: String,
        invite: String,
        reply: oneshot::Sender<WorkResult>,
    },
    RemoveContact {
        nickname: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendText {
        to: String,
        text: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendFile {
        to: String,
        path: PathBuf,
        reply: oneshot::Sender<WorkResult>,
    },
    SendFileBytes {
        to: String,
        name: String,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    Recv {
        reply: oneshot::Sender<WorkResult>,
    },
    Meta {
        reply: oneshot::Sender<WorkResult>,
    },
    Messages {
        reply: oneshot::Sender<WorkResult>,
    },
    Blob {
        id_hex: String,
        reply: oneshot::Sender<WorkResult>,
    },
    BackupExport {
        reply: oneshot::Sender<WorkResult>,
    },
    BackupImport {
        dir: PathBuf,
        data: Vec<u8>,
        reply: oneshot::Sender<WorkResult>,
    },
    RecoveryAck {
        reply: oneshot::Sender<WorkResult>,
    },
    RecoveryPending {
        reply: oneshot::Sender<WorkResult>,
    },
    HeartbeatPresence {
        reply: oneshot::Sender<WorkResult>,
    },
    ChangePassphrase {
        current: String,
        new: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SetMailHandle {
        handle: String,
        reply: oneshot::Sender<WorkResult>,
    },
    SendMail {
        to: String,
        thread_id: Option<String>,
        subject: Option<String>,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        reply: oneshot::Sender<WorkResult>,
    },
    /// Seal a message for someone with no Hush account.
    SendToOutsider {
        to: String,
        subject: String,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        secret: DropSecret,
        reply: oneshot::Sender<WorkResult>,
    },
    MailRecords {
        reply: oneshot::Sender<WorkResult>,
    },
    DraftRecords {
        reply: oneshot::Sender<WorkResult>,
    },
    SaveDraft {
        id: Option<String>,
        thread_id: Option<String>,
        to: String,
        subject: Option<String>,
        body: String,
        attachments: Vec<(String, Vec<u8>)>,
        reply: oneshot::Sender<WorkResult>,
    },
    DeleteDraft {
        id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    DeleteMail {
        id: String,
        reply: oneshot::Sender<WorkResult>,
    },
    RetryOutbox {
        reply: oneshot::Sender<WorkResult>,
    },
}

#[derive(Clone, serde::Serialize)]
pub struct ContactDto {
    pub nickname: String,
    /// The contact's account id.
    ///
    /// Still called `protocol_name` on the wire: it is the same value the UI has
    /// always keyed avatars and photos off, and renaming it would churn the UI
    /// for no behavioural gain. Multi-device made it the *account* identifier
    /// rather than a single device's, which is what it effectively already was.
    pub protocol_name: String,
    pub inbound_ready: bool,
    pub online: bool,
    /// This contact's mail handle (local part only), if they published one in
    /// their invite. The UI renders `handle@<domain>`.
    pub mail_handle: Option<String>,
}

pub enum WorkResult {
    Ok,
    Invite(String),
    Contacts(Vec<ContactDto>),
    Inbox(InboxResult),
    Meta {
        protocol_name: String,
        vault_bytes: u64,
        contacts: usize,
        recovery_acknowledged: bool,
        last_backup_unix: u64,
        relay: String,
        mail_handle: Option<String>,
    },
    Backup(Vec<u8>),
    RecoveryCode(String),
    Messages(Vec<ChatRecord>),
    /// The thread the mail was filed under, and whether the relay took it.
    MailSent { thread_id: String, queued: bool },
    /// A drop is on the relay: its capability id, and when it expires.
    OutsiderSent { delivery_id: String, expires_unix: u64 },
    Mail(Vec<ChatRecord>),
    Drafts(Vec<ChatRecord>),
    /// The record id of the saved draft.
    DraftSaved(String),
    Blob(Vec<u8>),
    BlobId(String),
    /// The supplied current passphrase did not match the vault.
    WrongPassphrase,
    Err(String),
}

impl AccountWorker {
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::channel::<Work>(64);
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("account worker runtime");
            rt.block_on(async move {
                let mut account: Option<Account> = None;
                while let Some(work) = rx.recv().await {
                    let (reply, result) = match work {
                        Work::Unlock { dir, passphrase, reply } => {
                            let result = match Account::open(&dir, &passphrase).await {
                                Ok(a) => {
                                    // Nothing to publish on unlock: peers learn
                                    // this account's inbox from its invite, and
                                    // there is no device list to refresh. A
                                    // rotated inbox cleared the cached invite in
                                    // `finish_open`, so the next invite mints a
                                    // fresh one.
                                    account = Some(a);
                                    WorkResult::Ok
                                }
                                Err(e) => WorkResult::Err(e.to_string()),
                            };
                            (reply, result)
                        }
                        Work::Lock { reply } => {
                            account = None;
                            (reply, WorkResult::Ok)
                        }
                        Work::Invite { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.invite().await {
                                    Ok(inv) => WorkResult::Invite(inv),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Contacts { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    // Presence reads the cached per-contact queue,
                                    // which only changes when a grant arrives, so
                                    // there is nothing to refresh here.
                                    let mut list = Vec::new();
                                    for c in a.contacts() {
                                        let online = a
                                            .contact_is_online(&c.nickname)
                                            .await
                                            .unwrap_or(false);
                                        list.push(ContactDto {
                                            nickname: c.nickname.clone(),
                                            protocol_name: c.account_id.clone(),
                                            inbound_ready: c.inbound_ready,
                                            online,
                                            mail_handle: c.mail_handle.clone(),
                                        });
                                    }
                                    WorkResult::Contacts(list)
                                }
                            };
                            (reply, result)
                        }
                        Work::HeartbeatPresence { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.heartbeat_presence().await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::AddContact {
                            nickname,
                            invite,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.add_contact(&nickname, &invite).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RemoveContact { nickname, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.revoke_contact(&nickname).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendText { to, text, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_text(&to, &text).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendFile { to, path, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_file(&to, &path).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendFileBytes {
                            to,
                            name,
                            data,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.send_file_bytes(&to, &name, &data).await {
                                    Ok(id) => WorkResult::BlobId(hex::encode(id.0)),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Recv { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.recv().await {
                                    Ok(inbox) => WorkResult::Inbox(inbox),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Meta { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Meta {
                                    protocol_name: a.protocol_name().to_string(),
                                    vault_bytes: a.vault_bytes_on_disk(),
                                    contacts: a.contacts().len(),
                                    recovery_acknowledged: a.recovery_acknowledged(),
                                    last_backup_unix: a.last_backup_unix(),
                                    relay: a.relay().to_string(),
                                    mail_handle: a.mail_handle().map(|h| h.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::Messages { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    WorkResult::Messages(a.chat_history())
                                }
                            };
                            (reply, result)
                        }
                        Work::BackupExport { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.export_backup().await {
                                    Ok(data) => WorkResult::Backup(data),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::BackupImport { dir, data, reply } => {
                            if account.is_some() {
                                (reply, WorkResult::Err("unlock vault before import".into()))
                            } else {
                                let result = match Account::import_backup(&dir, &data) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                };
                                (reply, result)
                            }
                        }
                        Work::RecoveryAck { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.acknowledge_recovery().await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RecoveryPending { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.take_pending_recovery_code() {
                                    Some(code) => WorkResult::RecoveryCode(code),
                                    None => WorkResult::Err("no pending recovery code".into()),
                                },
                            };
                            (reply, result)
                        }
                        Work::ChangePassphrase {
                            current,
                            new,
                            reply,
                        } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.change_passphrase(&current, &new) {
                                    Ok(()) => WorkResult::Ok,
                                    Err(hush_core::Error::WrongPassphrase) => {
                                        WorkResult::WrongPassphrase
                                    }
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SetMailHandle { handle, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.set_mail_handle(&handle).await {
                                    Ok(()) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendMail {
                            to,
                            thread_id,
                            subject,
                            body,
                            attachments,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .send_mail(
                                        &to,
                                        thread_id.as_deref(),
                                        subject.as_deref(),
                                        &body,
                                        &attachments,
                                    )
                                    .await
                                {
                                    Ok(outcome) => WorkResult::MailSent {
                                        thread_id: outcome.thread_id,
                                        queued: outcome.queued,
                                    },
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::SendToOutsider {
                            to,
                            subject,
                            body,
                            attachments,
                            secret,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    let sent = match &secret {
                                        DropSecret::Pin(pin) => {
                                            a.send_to_outsider(
                                                &to,
                                                &subject,
                                                &body,
                                                &attachments,
                                                pin,
                                            )
                                            .await
                                        }
                                        DropSecret::PublicKey(key) => {
                                            a.send_to_outsider_with_key(
                                                &to,
                                                &subject,
                                                &body,
                                                &attachments,
                                                key,
                                            )
                                            .await
                                        }
                                    };
                                    match sent {
                                        Ok(outcome) => WorkResult::OutsiderSent {
                                            delivery_id: outcome.delivery_id,
                                            expires_unix: outcome.expires_unix,
                                        },
                                        Err(e) => WorkResult::Err(e.to_string()),
                                    }
                                }
                            };
                            (reply, result)
                        }
                        Work::MailRecords { reply } => {                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Mail(a.mail_records()),
                            };
                            (reply, result)
                        }
                        Work::DraftRecords { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => WorkResult::Drafts(a.draft_records()),
                            };
                            (reply, result)
                        }
                        Work::SaveDraft {
                            id,
                            thread_id,
                            to,
                            subject,
                            body,
                            attachments,
                            reply,
                        } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a
                                    .save_draft(
                                        id.as_deref(),
                                        thread_id.as_deref(),
                                        &to,
                                        subject.as_deref(),
                                        &body,
                                        &attachments,
                                    )
                                    .await
                                {
                                    Ok(saved) => WorkResult::DraftSaved(saved),
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeleteDraft { id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.delete_draft(&id).await {
                                    Ok(_) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::DeleteMail { id, reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.delete_mail(&id).await {
                                    Ok(_) => WorkResult::Ok,
                                    Err(e) => WorkResult::Err(e.to_string()),
                                },
                            };
                            (reply, result)
                        }
                        Work::RetryOutbox { reply } => {
                            let result = match account.as_mut() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => match a.retry_outbox().await {
                                    // A failed flush is not worth surfacing: the
                                    // messages are still queued and the next tick
                                    // tries again.
                                    Ok(_) | Err(_) => WorkResult::Ok,
                                },
                            };
                            (reply, result)
                        }
                        Work::Blob { id_hex, reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    let bytes = hex::decode(&id_hex).map_err(|_| {
                                        WorkResult::Err("invalid blob id".into())
                                    });
                                    match bytes {
                                        Err(e) => e,
                                        Ok(b) if b.len() != 32 => {
                                            WorkResult::Err("invalid blob id".into())
                                        }
                                        Ok(b) => {
                                            let mut id = [0u8; 32];
                                            id.copy_from_slice(&b);
                                            match a.read_blob(&hush_core::BlobId(id)) {
                                                Ok(data) => WorkResult::Blob(data),
                                                Err(e) => WorkResult::Err(e.to_string()),
                                            }
                                        }
                                    }
                                }
                            };
                            (reply, result)
                        }
                    };
                    reply.send(result).ok();
                }
            });
        });
        Self { tx }
    }

    async fn call(&self, build: impl FnOnce(oneshot::Sender<WorkResult>) -> Work) -> WorkResult {
        let (reply, work) = {
            let (tx, rx) = oneshot::channel();
            (rx, build(tx))
        };
        if self.tx.send(work).await.is_err() {
            return WorkResult::Err("account worker stopped".into());
        }
        reply.await.unwrap_or_else(|_| WorkResult::Err("worker dropped".into()))
    }

    pub async fn unlock(&self, dir: &PathBuf, passphrase: &str) -> WorkResult {
        let dir = dir.clone();
        let passphrase = passphrase.to_string();
        self.call(|reply| Work::Unlock {
            dir,
            passphrase,
            reply,
        })
        .await
    }

    pub async fn lock(&self) -> WorkResult {
        self.call(|reply| Work::Lock { reply }).await
    }

    pub async fn invite(&self) -> WorkResult {
        self.call(|reply| Work::Invite { reply }).await
    }

    pub async fn contacts(&self) -> WorkResult {
        self.call(|reply| Work::Contacts { reply }).await
    }

    pub async fn add_contact(&self, nickname: &str, invite: &str) -> WorkResult {
        self.call(|reply| Work::AddContact {
            nickname: nickname.to_string(),
            invite: invite.to_string(),
            reply,
        })
        .await
    }

    pub async fn remove_contact(&self, nickname: &str) -> WorkResult {
        self.call(|reply| Work::RemoveContact {
            nickname: nickname.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_text(&self, to: &str, text: &str) -> WorkResult {
        self.call(|reply| Work::SendText {
            to: to.to_string(),
            text: text.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_file(&self, to: &str, path: &PathBuf) -> WorkResult {
        self.call(|reply| Work::SendFile {
            to: to.to_string(),
            path: path.clone(),
            reply,
        })
        .await
    }

    pub async fn send_file_bytes(&self, to: &str, name: &str, data: Vec<u8>) -> WorkResult {
        self.call(|reply| Work::SendFileBytes {
            to: to.to_string(),
            name: name.to_string(),
            data,
            reply,
        })
        .await
    }

    pub async fn recv(&self) -> WorkResult {
        self.call(|reply| Work::Recv { reply }).await
    }

    pub async fn meta(&self) -> WorkResult {
        self.call(|reply| Work::Meta { reply }).await
    }

    pub async fn messages(&self) -> WorkResult {
        self.call(|reply| Work::Messages { reply }).await
    }

    pub async fn blob(&self, id_hex: &str) -> WorkResult {
        self.call(|reply| Work::Blob {
            id_hex: id_hex.to_string(),
            reply,
        })
        .await
    }

    pub async fn backup_export(&self) -> WorkResult {
        self.call(|reply| Work::BackupExport { reply }).await
    }

    pub async fn backup_import(&self, dir: &PathBuf, data: Vec<u8>) -> WorkResult {
        let dir = dir.clone();
        self.call(|reply| Work::BackupImport {
            dir,
            data,
            reply,
        })
        .await
    }

    pub async fn recovery_ack(&self) -> WorkResult {
        self.call(|reply| Work::RecoveryAck { reply }).await
    }

    pub async fn recovery_pending(&self) -> WorkResult {
        self.call(|reply| Work::RecoveryPending { reply }).await
    }

    pub async fn heartbeat_presence(&self) -> WorkResult {
        self.call(|reply| Work::HeartbeatPresence { reply }).await
    }

    pub async fn change_passphrase(&self, current: &str, new: &str) -> WorkResult {
        self.call(|reply| Work::ChangePassphrase {
            current: current.to_string(),
            new: new.to_string(),
            reply,
        })
        .await
    }

    pub async fn set_mail_handle(&self, handle: &str) -> WorkResult {
        self.call(|reply| Work::SetMailHandle {
            handle: handle.to_string(),
            reply,
        })
        .await
    }

    pub async fn send_mail(
        &self,
        to: &str,
        thread_id: Option<String>,
        subject: Option<String>,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
    ) -> WorkResult {
        self.call(|reply| Work::SendMail {
            to: to.to_string(),
            thread_id,
            subject,
            body: body.to_string(),
            attachments,
            reply,
        })
        .await
    }

    pub async fn mail_records(&self) -> WorkResult {
        self.call(|reply| Work::MailRecords { reply }).await
    }

    /// Seal a message for an address with no Hush account and park it on the
    /// relay. Returns the capability id and expiry, not a link: the base URL is
    /// daemon configuration. `secret` selects the PIN or public-key mode.
    pub async fn send_to_outsider(
        &self,
        to: &str,
        subject: &str,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
        secret: DropSecret,
    ) -> WorkResult {
        self.call(|reply| Work::SendToOutsider {
            to: to.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            attachments,
            secret,
            reply,
        })
        .await
    }
    pub async fn draft_records(&self) -> WorkResult {
        self.call(|reply| Work::DraftRecords { reply }).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_draft(
        &self,
        id: Option<String>,
        thread_id: Option<String>,
        to: &str,
        subject: Option<String>,
        body: &str,
        attachments: Vec<(String, Vec<u8>)>,
    ) -> WorkResult {
        self.call(|reply| Work::SaveDraft {
            id,
            thread_id,
            to: to.to_string(),
            subject,
            body: body.to_string(),
            attachments,
            reply,
        })
        .await
    }

    pub async fn delete_draft(&self, id: &str) -> WorkResult {
        self.call(|reply| Work::DeleteDraft {
            id: id.to_string(),
            reply,
        })
        .await
    }

    pub async fn delete_mail(&self, id: &str) -> WorkResult {
        self.call(|reply| Work::DeleteMail {
            id: id.to_string(),
            reply,
        })
        .await
    }

    /// Flush the Outbox. Called on a timer, not by the UI.
    pub async fn retry_outbox(&self) -> WorkResult {
        self.call(|reply| Work::RetryOutbox { reply }).await
    }
}
