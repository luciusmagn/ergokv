use std::process::{Child, Command};
use std::thread::sleep;
use std::time::Duration;

struct TiKVTestInstance {
    pd_process: Child,
    tikv_process: Child,
}

impl TiKVTestInstance {
    fn new() -> Self {
        // Start PD (Placement Driver)
        let pd_process = Command::new("pd-server")
            .args(&[
                "--data-dir",
                "/tmp/pd",
                "--client-urls",
                "http://127.0.0.1:2379",
            ])
            .spawn()
            .expect("Failed to start PD");

        // Give PD a moment to start
        sleep(Duration::from_secs(2));

        // Start TiKV
        let tikv_process = Command::new("tikv-server")
            .args(&[
                "--pd",
                "127.0.0.1:2379",
                "--data-dir",
                "/tmp/tikv",
            ])
            .spawn()
            .expect("Failed to start TiKV");

        // Give TiKV a moment to start
        sleep(Duration::from_secs(5));

        TiKVTestInstance {
            pd_process,
            tikv_process,
        }
    }
}

impl Drop for TiKVTestInstance {
    fn drop(&mut self) {
        // Stop TiKV and PD
        self.tikv_process
            .kill()
            .expect("Failed to kill TiKV process");
        self.pd_process
            .kill()
            .expect("Failed to kill PD process");
    }
}

// The good part of the test starts here:
use ergokv::Store;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Store, Serialize, Deserialize, Debug, PartialEq)]
struct User {
    #[key]
    id: Uuid,
    #[index]
    username: String,
    email: String,
}

#[tokio::test]
async fn test_user_store() {
    // Start TiKV instance
    let _tikv_instance = TiKVTestInstance::new();

    // Set up TiKV client
    let client = tikv_client::TransactionClient::new(vec![
        "127.0.0.1:2379",
    ])
    .await
    .unwrap();

    // Create a new user
    let user = User {
        id: Uuid::new_v4(),
        username: "testuser".to_string(),
        email: "test@example.com".to_string(),
    };

    // Start a transaction
    let mut txn = client.begin_optimistic().await.unwrap();

    // Save the user
    user.save(&mut txn).await.unwrap();

    // Commit the transaction
    txn.commit().await.unwrap();

    // Start a new transaction
    let mut txn = client.begin_optimistic().await.unwrap();

    // Load the user
    let mut loaded_user =
        User::load(&user.id, &mut txn).await.unwrap();

    // Assert that the loaded user matches the original
    assert_eq!(user, loaded_user);

    // Test index method
    let found_user = User::by_username(&user.username, &mut txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user, found_user);

    // Update user
    loaded_user
        .set_email("newemail@example.com".to_string(), &mut txn)
        .await
        .unwrap();

    // Delete user
    loaded_user.delete(&mut txn).await.unwrap();

    // Commit the transaction
    txn.commit().await.unwrap();

    // Try to load the deleted user (should fail)
    let mut txn = client.begin_optimistic().await.unwrap();
    assert!(User::load(&user.id, &mut txn).await.is_err());

    txn.commit().await.unwrap();
}
