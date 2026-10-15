use super::*;

impl DeviceNetworkService {
    pub(crate) async fn shared_conversation_services(
        &self,
        attempt_id: &str,
        generation: u64,
    ) -> Result<Value, String> {
        if !super::super::stable_id(attempt_id) || generation == 0 {
            return Err("Invalid shared attempt identity".into());
        }
        let connection = self
            .shared_connection()
            .ok_or_else(|| "Hub connection is unavailable on this Runner".to_string())?;
        let generation_text = generation.to_string();
        let services: Value = request(
            &connection.client,
            "runner/conversation-services",
            None,
            None,
            &[("attempt_id", attempt_id), ("generation", &generation_text)],
        )
        .await
        .map_err(|error| error.message().to_owned())?;
        if services
            .as_array()
            .is_none_or(|services| services.len() > 128)
        {
            return Err("Hub returned an invalid conversation service list".into());
        }
        Ok(services)
    }

    /// The Hub checks the current Runner attempt and conversation before requesting
    /// that another Runner stop an exact retained process. A successful response is
    /// only a stop request, never proof of process drain.
    pub(crate) async fn shared_conversation_stop(
        &self,
        service_id: &str,
        attempt_id: &str,
        generation: u64,
    ) -> Result<Value, String> {
        if !super::super::stable_id(service_id)
            || !super::super::stable_id(attempt_id)
            || generation == 0
        {
            return Err("Invalid retained service or shared attempt identity".into());
        }
        let connection = self
            .shared_connection()
            .ok_or_else(|| "Hub connection is unavailable on this Runner".to_string())?;
        let value: Value = request(
            &connection.client,
            &format!("runner/services/{service_id}/conversation-stop"),
            None,
            Some(json!({"attempt_id":attempt_id,"generation":generation})),
            &[],
        )
        .await
        .map_err(|error| error.message().to_owned())?;
        if value.get("service_id").and_then(Value::as_str) != Some(service_id)
            || value.get("stop_requested").and_then(Value::as_bool) != Some(true)
        {
            return Err("Hub did not confirm this exact service stop request".into());
        }
        Ok(value)
    }
}
