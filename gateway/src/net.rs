use crate::{router, state::App};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinSet};

pub async fn serve(app: Arc<App>) -> anyhow::Result<()> {
    let listener = TcpListener::bind((app.args.host, app.args.port)).await?;
    app.log(
        "INFO",
        &format!(
            "wb2api listening at http://{}:{} (baseline 477ba36; selected upstream fixes 4f7ae6f)",
            app.args.host, app.args.port
        ),
    );
    let mut stop = app.shutdown.subscribe();
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            joined = connections.join_next(), if !connections.is_empty() => { if let Some(Err(_))=joined { app.log("WARN","connection task ended unexpectedly"); } },
            accepted=listener.accept() => {
                let (socket,peer)=accepted?; socket.set_nodelay(true)?;
                let app=app.clone(); let mut quit=app.shutdown.subscribe();
                connections.spawn(async move {
                    let service=service_fn(move |request| { let a=app.clone(); async move {Ok::<_,Infallible>(router::route(a,request,peer).await)} });
                    let mut builder=http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(30)).max_buf_size(1024*1024).keep_alive(true);
                    let connection=builder.serve_connection(TokioIo::new(socket),service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = connection.as_mut() => {},
                        _ = quit.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _=tokio::time::timeout(Duration::from_secs(30),connection.as_mut()).await;
                        }
                    }
                });
            }
        }
    }
    drop(listener);
    let drain = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(Duration::from_secs(35), drain)
        .await
        .is_err()
    {
        connections.abort_all();
    }
    Ok(())
}

pub async fn wait_for_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
