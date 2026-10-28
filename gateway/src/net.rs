use crate::{router, state::App};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, io, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

/// Errors that concern one pending connection only. Anything else (EMFILE,
/// ENFILE, ENOBUFS, ENOMEM) is resource exhaustion: back off briefly instead
/// of spinning, and never exit the server.
fn per_connection(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    )
}

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
    let mut accept_errors = 0u64;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(e)) = joined {
                    if e.is_panic() {
                        app.log("ERROR", "connection task panicked");
                    }
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((socket, peer)) => {
                    accept_errors = 0;
                    connections.spawn(connection(app.clone(), socket, peer));
                }
                Err(e) if per_connection(&e) => {}
                Err(e) => {
                    accept_errors += 1;
                    if accept_errors == 1 || accept_errors.is_multiple_of(100) {
                        app.log("ERROR", &format!("accept failed ({accept_errors} in a row): {e}"));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
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
    // Usage rows are billing records: give queued appends a bounded chance to land.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app
        .pending_writes
        .load(std::sync::atomic::Ordering::Acquire)
        > 0
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

async fn connection(app: Arc<App>, socket: TcpStream, peer: std::net::SocketAddr) {
    // Nagle only delays small SSE frames; failure here is harmless.
    let _ = socket.set_nodelay(true);
    let mut quit = app.shutdown.subscribe();
    let service = service_fn(move |request| {
        let a = app.clone();
        async move { Ok::<_, Infallible>(router::route(a, request, peer).await) }
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .max_buf_size(1024 * 1024)
        .keep_alive(true);
    let connection = builder.serve_connection(TokioIo::new(socket), service);
    tokio::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => {},
        _ = quit.changed() => {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(30), connection.as_mut()).await;
        }
    }
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
