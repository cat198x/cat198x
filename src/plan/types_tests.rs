use super::*;

#[test]
fn test_plan_new() {
    let plan = Plan::new("abc123".to_string());
    assert_eq!(plan.state_hash, "abc123");
    assert!(plan.is_empty());
    assert_eq!(plan.operation_count(), 0);
}

#[test]
fn test_plan_add_copy() {
    let mut plan = Plan::new("test".to_string());

    plan.add_copy(
        SourceRef {
            path: "/source/game.rom".to_string(),
            archive_path: None,
            sha1: "ABC123".to_string(),
            entry_name: None,
        },
        "/dest/game.rom".to_string(),
        1024,
    );

    assert!(!plan.is_empty());
    assert_eq!(plan.operation_count(), 1);
    assert_eq!(plan.summary.copy_count, 1);
    assert_eq!(plan.summary.total_bytes, 1024);
}

#[test]
fn test_plan_serialize() {
    let mut plan = Plan::new("hash123".to_string());
    plan.add_copy(
        SourceRef {
            path: "/src/rom.nes".to_string(),
            archive_path: None,
            sha1: "SHA1HASH".to_string(),
            entry_name: None,
        },
        "/dest/rom.nes".to_string(),
        2048,
    );

    let json = serde_json::to_string_pretty(&plan).unwrap();
    assert!(json.contains("\"state_hash\": \"hash123\""));
    assert!(json.contains("\"type\": \"copy\""));
    assert!(json.contains("\"/src/rom.nes\""));
}

#[test]
fn test_plan_deserialize() {
    let json = r#"{
            "state_hash": "test123",
            "created_at": "2024-01-01 00:00:00",
            "operations": [
                {
                    "id": 0,
                    "status": "pending",
                    "kind": {
                        "type": "copy",
                        "source": {
                            "path": "/src/file.rom",
                            "archive_path": null,
                            "sha1": "DEADBEEF"
                        },
                        "dest": "/dest/file.rom",
                        "size": 1000
                    }
                }
            ],
            "summary": {
                "copy_count": 1,
                "move_count": 0,
                "repack_count": 0,
                "delete_count": 0,
                "already_correct": 0,
                "missing": 0,
                "total_bytes": 1000
            }
        }"#;

    let plan: Plan = serde_json::from_str(json).unwrap();
    assert_eq!(plan.state_hash, "test123");
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(plan.summary.copy_count, 1);
}

#[test]
fn test_operation_kind_copy() {
    let kind = OperationKind::Copy {
        source: SourceRef {
            path: "/src".to_string(),
            archive_path: None,
            sha1: "hash".to_string(),
            entry_name: None,
        },
        dest: "/dest".to_string(),
        size: 100,
        placement: CopyPlacement::LooseFile,
    };

    let json = serde_json::to_string(&kind).unwrap();
    assert!(json.contains("\"type\":\"copy\""));
}

#[test]
fn test_operation_kind_repack() {
    let kind = OperationKind::Repack {
        sources: vec![
            SourceRef {
                path: "/src/a.rom".to_string(),
                archive_path: None,
                sha1: "hash1".to_string(),
                entry_name: None,
            },
            SourceRef {
                path: "/src/b.rom".to_string(),
                archive_path: None,
                sha1: "hash2".to_string(),
                entry_name: None,
            },
        ],
        dest: "/dest/game.zip".to_string(),
        format: "zip".to_string(),
        size: 2048,
        move_sources: false,
    };

    let json = serde_json::to_string(&kind).unwrap();
    assert!(json.contains("\"type\":\"repack\""));
    assert!(json.contains("\"sources\""));
}
