use std::path::PathBuf;
use std::thread;

use hush_core::{Account, ChatRecord, Contact, InboxResult};
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
}

pub enum WorkResult {
    Ok,
    Invite(String),
    Contacts(Vec<Contact>),
    Inbox(InboxResult),
    Meta {
        protocol_name: String,
        vault_bytes: u64,
        contacts: usize,
    },
    Messages(Vec<ChatRecord>),
    Blob(Vec<u8>),
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
                                Some(a) => WorkResult::Contacts(a.contacts().to_vec()),
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
                                    Ok(()) => WorkResult::Ok,
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
}
