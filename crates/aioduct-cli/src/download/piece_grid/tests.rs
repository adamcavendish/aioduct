use super::*;

#[test]
fn piece_grid_columns_prefers_available_width() {
    assert_eq!(piece_grid_columns(60, 40), 40);
    assert_eq!(piece_grid_columns(10, 40), 9);
    assert_eq!(piece_grid_columns(1, 40), 1);
    assert_eq!(piece_grid_columns(60, 0), 1);
}

#[test]
fn piece_grid_panel_height_stays_compact_for_small_piece_counts() {
    assert_eq!(piece_grid_panel_height(72, 80), PIECE_DETAIL_MIN_HEIGHT);
    assert_eq!(piece_grid_panel_height(72, 976), PIECE_GRID_MAX_HEIGHT);
}

#[test]
fn piece_visual_state_tracks_design_breakpoints() {
    assert_eq!(piece_visual_state(8), PieceVisualState::Small);
    assert_eq!(piece_visual_state(32), PieceVisualState::Small);
    assert_eq!(piece_visual_state(33), PieceVisualState::Medium);
    assert_eq!(piece_visual_state(128), PieceVisualState::Medium);
    assert_eq!(piece_visual_state(129), PieceVisualState::Large);
}

#[test]
fn small_piece_grid_uses_expanded_cells() {
    assert_eq!(piece_grid_cells_per_row(24, 8), 8);
    assert_eq!(piece_grid_cells_per_row(9, 8), 3);
    assert_eq!(piece_grid_cells_per_row(24, 80), 23);
}

#[test]
fn piece_size_policy_label_names_auto_bounds_and_overrides() {
    assert_eq!(piece_size_policy_label(64 * 1024), "piece auto 64 KiB min");
    assert_eq!(
        piece_size_policy_label(4 * 1024 * 1024),
        "piece auto 4 MiB max"
    );
    assert_eq!(
        piece_size_policy_label(16 * 1024 * 1024),
        "piece override 16 MiB"
    );
}

#[test]
fn piece_viewport_reports_actual_visible_range() {
    let pieces: Vec<PieceSnapshot> = (0..256)
        .map(|index| PieceSnapshot {
            index,
            state: PieceState::Pending,
            retry_count: 0,
            last_error: None,
        })
        .collect();
    let params = HeatMapParams {
        pieces: &pieces,
        total_pieces: 256,
        piece_length: 4 * 1024 * 1024,
        scroll_offset: 0,
        frame_count: 0,
        selected_piece: None,
        viewport: None,
    };
    let viewport = piece_viewport_for_inner(49, 2, &params);
    assert_eq!(viewport.first_piece, 0);
    assert_eq!(viewport.last_piece, 47);

    let scrolled = HeatMapParams {
        scroll_offset: 1,
        ..params
    };
    let viewport = piece_viewport_for_inner(49, 2, &scrolled);
    assert_eq!(viewport.first_piece, 48);
    assert_eq!(viewport.last_piece, 95);
}
