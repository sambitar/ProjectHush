use std::path::PathBuf;
use std::thread;

use hush_core::{Account, ChatRecord, InboxResult};
use tokio::sync::{mpsc, oneshot};

pub struct AccountWorker {
    tx: mpsc::Sender<Work>,
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
    },
    Backup(Vec<u8>),
    RecoveryCode(String),
    Messages(Vec<ChatRecord>),
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
                                Ok(mut a) => {
                                    // A vault written before the queue credential
                                    // split comes back with a replaced inbox. If
                                    // we do not republish, peers keep delivering
                                    // to the abandoned one and this device goes
                                    // silent. Best effort: a relay that is down
                                    // must not block an unlock, and the next
                                    // unlock retries.
                                    if a.queue_rotated() {
                                        if let Err(e) = a.publish_device_list().await {
                                            eprintln!(
                                                "hush: could not republish device list after \
                                                 queue rotation: {e}"
                                            );
                                        }
                                    }
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
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
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
                                Some(a) => match a.remove_contact(&nickname).await {
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
                                },
                            };
                            (reply, result)
                        }
                        Work::Messages { reply } => {
                            let result = match account.as_ref() {
                                None => WorkResult::Err("locked".into()),
                                Some(a) => {
                                    WorkResult::Messages(a.chat_history().to_vec())
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
}
