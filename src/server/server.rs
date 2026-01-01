use tracing::error;
use tokio_stream::StreamExt;
use tonic::{transport::Server as TonicServer, Request, Response, Status, Streaming};
use futures::stream;

use crate::errors::{Result, Error, ErrorKind};
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    ScrollEvent, ClipboardEvent,
};

#[derive(Debug, Default)]
pub struct Server {}

#[tonic::async_trait]
impl SynqService for Server {
    type ScrollStream = stream::Empty<Result<ScrollEvent, Status>>;
    type ClipboardStream = stream::Empty<Result<ClipboardEvent, Status>>;

    async fn scroll(
        &self,
        request: Request<Streaming<ScrollEvent>>,
    ) -> Result<Response<Self::ScrollStream>, Status> {
        println!("Scroll connection established");

        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(evt) => {
                        println!("{} - {}", evt.delta_x, evt.delta_y)
                    }
                    Err(e) => {
                        let err = Error::wrap(e, ErrorKind::Network)
                            .with_msg("server: Failed to read scroll event");
                        error!(?err);
                        break;
                    }
                }
            }
            println!("Scroll connection closed");
        });

        Ok(Response::new(stream::empty()))
    }

    async fn clipboard(
        &self,
        request: Request<Streaming<ClipboardEvent>>,
    ) -> Result<Response<Self::ClipboardStream>, Status> {
        println!("Clipboard connection established");

        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(event) => {
                        println!("Clipboard client={} data={}",
                            event.client, event.data.len());
                    }
                    Err(e) => {
                        let err = Error::wrap(e, ErrorKind::Network)
                            .with_msg("server: Failed to read clipboard event");
                        error!(?err);
                        break;
                    }
                }
            }
            println!("Clipboard connection closed");
        });

        Ok(Response::new(stream::empty()))
    }
}

impl Server {
    pub async fn run(bind: String) -> Result<()> {
        let addr = bind.parse()
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("server: Failed to parse address")
            )?;
        let server = Server::default();

        println!("Synq server listening on {}", addr);

        TonicServer::builder()
            .add_service(SynqServiceServer::new(server))
            .serve(addr)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("server: Failed to run server")
            )?;

        Ok(())
    }
}
