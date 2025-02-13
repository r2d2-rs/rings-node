use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use super::prepare_node;
use crate::dht::successor::SuccessorReader;
use crate::dht::Chord;
use crate::dht::Stabilization;
use crate::dht::TStabilize;
use crate::ecc::SecretKey;
use crate::error::Error;
use crate::error::Result;
use crate::inspect::DHTInspect;
use crate::swarm::tests::new_swarm;
use crate::swarm::Swarm;
use crate::tests::default::gen_pure_dht;
use crate::tests::manually_establish_connection;
use crate::transports::manager::TransportManager;

async fn run_stabilize(swarm: Arc<Swarm>) {
    let mut result = Result::<()>::Ok(());
    let stabilization = Stabilization::new(swarm, 5usize);
    let timeout_in_secs = stabilization.get_timeout();
    println!("RUN Stabilization");
    while result.is_ok() {
        let timeout = sleep(Duration::from_secs(timeout_in_secs as u64));
        tokio::pin!(timeout);
        tokio::select! {
            _ = timeout.as_mut() => {
                result = stabilization
                    .stabilize()
                    .await;
            }
        }
    }
}

async fn run_node(swarm: Arc<Swarm>) {
    let message_handler = async { swarm.clone().listen().await };

    let stb = Stabilization::new(swarm.clone(), 3);
    let stabilization = async { Arc::new(stb).wait().await };

    futures::future::join(message_handler, stabilization).await;
}

#[tokio::test]
async fn test_stabilization_once() -> Result<()> {
    let mut key1 = SecretKey::random();
    let mut key2 = SecretKey::random();
    // key 2 > key 1 here
    if key1.address() < key2.address() {
        (key1, key2) = (key2, key1)
    }
    let swarm1 = Arc::new(new_swarm(key1).await?);
    let swarm2 = Arc::new(new_swarm(key2).await?);
    manually_establish_connection(&swarm1, &swarm2).await?;
    println!("swarm1: {:?}, swarm2: {:?}", swarm1.did(), swarm2.did());

    tokio::select! {
        _ = async {
            futures::join!(
                async {
                    loop {
                        swarm1.clone().listen().await;
                    }
                },
                async {
                    loop {
                        swarm2.clone().listen().await;
                    }
                },
            );
        } => { unreachable!(); }
        _ = async {
            let transport_1_to_2 = swarm1.get_transport(swarm2.did()).unwrap();
            sleep(Duration::from_millis(1000)).await;
            transport_1_to_2.wait_for_data_channel_open().await.unwrap();
            assert!(swarm1.dht().successors().list()?.contains(&key2.address().into()));
            assert!(swarm2.dht().successors().list()?.contains(&key1.address().into()));
            let stabilization = Stabilization::new(Arc::clone(&swarm1), 5usize);
            let _ = stabilization.stabilize().await;
            sleep(Duration::from_millis(10000)).await;
            assert_eq!(*swarm2.dht().lock_predecessor()?, Some(key1.address().into()));
            assert!(swarm1.dht().successors().list()?.contains(&key2.address().into()));

            Ok::<(), Error>(())
        } => {}
    }
    tokio::fs::remove_dir_all("./tmp").await.ok();
    Ok(())
}

#[tokio::test]
async fn test_stabilization() -> Result<()> {
    let mut key1 = SecretKey::random();
    let mut key2 = SecretKey::random();
    // key 2 > key 1 here
    if key1.address() < key2.address() {
        (key1, key2) = (key2, key1)
    }
    let swarm1 = Arc::new(new_swarm(key1).await?);
    let swarm2 = Arc::new(new_swarm(key2).await?);
    manually_establish_connection(&swarm1, &swarm2).await?;

    tokio::select! {
        _ = async {
            tokio::join!(
                async {
                    loop {
                        swarm1.clone().listen().await;
                    }
                },
                async {
                    loop {
                        swarm2.clone().listen().await;
                    }
                },
                async {
                    run_stabilize(Arc::clone(&swarm1)).await;
                },
                async {
                    run_stabilize(Arc::clone(&swarm2)).await;
                }
            );
        } => { unreachable!(); }
        _ = async {
            let transport_1_to_2 = swarm1.get_transport(swarm2.did()).unwrap();
            sleep(Duration::from_millis(1000)).await;
            transport_1_to_2.wait_for_data_channel_open().await.unwrap();
            assert!(swarm1.dht().successors().list()?.contains(&key2.address().into()));
            assert!(swarm2.dht().successors().list()?.contains(&key1.address().into()));
            sleep(Duration::from_millis(10000)).await;
            assert_eq!(*swarm2.dht().lock_predecessor()?, Some(key1.address().into()));
            assert_eq!(*swarm1.dht().lock_predecessor()?, Some(key2.address().into()));
            Ok::<(), Error>(())
        } => {}
    }
    tokio::fs::remove_dir_all("./tmp").await.ok();
    Ok(())
}

#[ignore]
#[tokio::test]
async fn test_online_stabilization() -> Result<()> {
    let mut nodes = vec![];

    for _ in 0..6 {
        let key = SecretKey::random();
        let (swarm, _) = prepare_node(key).await;
        nodes.push(swarm.clone());
        tokio::spawn(async { run_node(swarm).await });
    }

    let swarm1 = Arc::clone(&nodes[0]);
    for swarm in nodes.iter().skip(1) {
        manually_establish_connection(&swarm1, swarm).await.unwrap();
    }

    sleep(Duration::from_secs(30)).await;

    let mut expected_dhts = vec![];
    for node in nodes.iter() {
        let dht = gen_pure_dht(node.did()).await.unwrap();
        for other in nodes.iter() {
            if dht.did != other.did() {
                dht.join(other.did()).unwrap();
                dht.notify(other.did()).unwrap();
            }
        }

        expected_dhts.push(DHTInspect::inspect(&dht));
    }

    let mut current_dhts = vec![];
    for node in nodes.iter() {
        current_dhts.push(DHTInspect::inspect(&node.dht()));
    }

    assert_eq!(expected_dhts, current_dhts);

    Ok(())
}
