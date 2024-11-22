use ergokv::{LocalCluster, Store};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use uuid::Uuid;

#[derive(
    Store, Serialize, Deserialize, Debug, PartialEq, Clone,
)]
struct User {
    #[key]
    id: Uuid,
    #[unique_index]
    username: String,
    #[index]
    email: String,
    #[index]
    department: String,
}

#[tokio::test]
async fn test_user_store() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    // Start TiKV instance
    let tikv_instance = LocalCluster::start(tmp.path()).unwrap();

    // Set up TiKV client
    let client = tikv_instance.spawn_client().await.unwrap();

    // Create a new user
    let user = User {
        id: Uuid::new_v4(),
        username: "testuser".to_string(),
        email: "test@example.com".to_string(),
        department: "Engineering".to_string(),
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

    // Test unique index method
    let found_user = User::by_username(&user.username, &mut txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user, found_user);

    // Test non-unique index method
    let users_by_email =
        User::by_email(&user.email, &mut txn).await.unwrap();
    assert_eq!(users_by_email.len(), 1);
    assert_eq!(users_by_email[0], user);

    let users_by_department =
        User::by_department(&user.department, &mut txn)
            .await
            .unwrap();
    assert_eq!(users_by_department.len(), 1);
    assert_eq!(users_by_department[0], user);

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

#[tokio::test]
async fn test_user_all() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let tikv_instance = LocalCluster::start(tmp.path()).unwrap();
    let client = tikv_instance.spawn_client().await.unwrap();
    let mut txn = client.begin_optimistic().await.unwrap();

    // Create test users
    let users = vec![
        User {
            id: Uuid::new_v4(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            department: "Engineering".to_string(),
        },
        User {
            id: Uuid::new_v4(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            department: "Engineering".to_string(),
        },
        User {
            id: Uuid::new_v4(),
            username: "charlie".to_string(),
            email: "charlie@example.com".to_string(),
            department: "Marketing".to_string(),
        },
    ];

    // Save all users
    for user in &users {
        user.save(&mut txn).await.unwrap();
    }
    txn.commit().await.unwrap();

    // Test index methods
    let mut txn = client.begin_optimistic().await.unwrap();

    // Unique index method
    let bob = User::by_username("bob", &mut txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob, users[1]);

    // Non-unique index method
    let engineering_users =
        User::by_department("Engineering", &mut txn)
            .await
            .unwrap();
    assert_eq!(engineering_users.len(), 2);

    // Test all() method
    let mut found_users = Vec::new();

    {
        let stream = User::all(&mut txn);
        futures::pin_mut!(stream);
        while let Some(Ok(user)) = stream.next().await {
            found_users.push(user);
        }
    }

    // Sort both vectors by username for comparison
    let mut users = users.clone();
    users.sort_by(|a, b| a.username.cmp(&b.username));
    found_users.sort_by(|a, b| a.username.cmp(&b.username));

    assert_eq!(users, found_users);
    txn.commit().await.unwrap();
}
