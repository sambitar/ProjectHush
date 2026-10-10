use std::sync::Arc;

use crate::App;

pub fn spawn_suspend_lock_watch(state: Arc<App>) {
    #[cfg(target_os = "linux")]
    {
        tokio::spawn(async move {
            if let Err(e) = watch_prepare_for_sleep(state).await {
                eprintln!("hush-daemon: suspend auto-lock unavailable ({e})");
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
    }
}

#[cfg(target_os = "linux")]
async fn watch_prepare_for_sleep(
    state: Arc<App>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futures_util::StreamExt;
    use zbus::message::Type;
    use zbus::{Connection, MatchRule, MessageStream};

    let connection = Connection::system().await?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.login1")?
        .interface("org.freedesktop.login1.Manager")?
        .member("PrepareForSleep")?
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &connection, None).await?;
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let sleeping: bool = msg.body().deserialize()?;
        if sleeping {
            crate::force_lock(&state).await;
            eprintln!("hush-daemon: auto-locked for system suspend");
        }
    }
    Ok(())
}
