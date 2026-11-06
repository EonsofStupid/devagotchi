//! Waiting until the broker runs no jobs before an update replaces it (spec U3 step 1):
//! `OperationList` over the same broker connection the link uses for calls.

use std::{future::Future, path::PathBuf, time::Duration};

use devagotchi_contracts::{ListInput, OperationState, RequestBody, ResponseBody};

/// Wait until `busy` reports no running jobs: `waiting` each time it does, again every
/// `recheck`, and `failed: busy` once `give_up` has passed.
pub(crate) async fn wait_for_idle<F, Fut>(
    mut busy: F,
    recheck: Duration,
    give_up: Duration,
    mut waiting: impl FnMut(),
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<usize, String>>,
{
    let started = tokio::time::Instant::now();
    loop {
        if busy().await? == 0 {
            return Ok(());
        }
        if started.elapsed() >= give_up {
            return Err("busy".into());
        }
        waiting();
        tokio::time::sleep(recheck).await;
    }
}

/// How many of the broker's operations are accepted or running, from `OperationList`.
pub(crate) async fn busy_operations(broker: PathBuf) -> Result<usize, String> {
    let (mut offset, mut busy) = (0, 0);
    loop {
        let body = RequestBody::OperationList(ListInput { offset, limit: 64 });
        match devagotchi_ipc::request(&broker, body).await {
            Ok(ResponseBody::Operations(page)) => {
                busy += page
                    .operations
                    .iter()
                    .filter(|o| {
                        matches!(o.state, OperationState::Accepted | OperationState::Running)
                    })
                    .count();
                match page.next_offset {
                    Some(next) if next > offset => offset = next,
                    _ => return Ok(busy),
                }
            }
            Ok(_) => return Err("the broker answered OperationList with something else".into()),
            Err(error) => return Err(format!("the broker: {error}")),
        }
    }
}
