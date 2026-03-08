use tonic::Status;

use crate::proto::agent::automation_service_client::AutomationServiceClient;
use crate::proto::agent::CancelUpdateRequest;

/// Send CancelUpdate RPC to force-unlock a stale Pulumi backend lock.
pub async fn cancel_update(channel: &tonic::transport::Channel) -> Result<String, Status> {
    let mut client = AutomationServiceClient::new(channel.clone())
        .max_decoding_message_size(16 * 1024 * 1024)
        .max_encoding_message_size(4 * 1024 * 1024);

    let response = client.cancel_update(CancelUpdateRequest {}).await?;

    Ok(response.into_inner().message)
}
