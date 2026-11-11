use crate::region::Region;

#[test]
fn full_region_covers_entire_grid() {
    // Arrange
    let cols = 80;
    let rows = 24;

    // Act
    let region = Region::full(cols, rows);

    // Assert
    assert_eq!(region, Region::new(0, 0, 80, 24));
}

#[test]
fn top_row_spans_full_width_one_row() {
    // Arrange / Act
    let region = Region::top_row(120);

    // Assert
    assert_eq!(region.height, 1);
    assert_eq!(region.width, 120);
    assert_eq!(region.row, 0);
}

#[test]
fn footer_is_last_row() {
    // Arrange / Act
    let region = Region::footer(80, 24);

    // Assert
    assert_eq!(region.row, 23);
    assert_eq!(region.height, 1);
}

#[test]
fn contains_checks_bounds() {
    // Arrange
    let region = Region::new(10, 5, 20, 10);

    // Act / Assert
    assert!(region.contains(10, 5));
    assert!(region.contains(29, 14));
    assert!(!region.contains(9, 5));
    assert!(!region.contains(30, 5));
    assert!(!region.contains(10, 15));
}

#[test]
fn encloses_checks_full_containment() {
    // Arrange
    let outer = Region::new(0, 0, 80, 24);
    let inner = Region::new(10, 5, 20, 10);
    let outside = Region::new(70, 20, 20, 10);

    // Act / Assert
    assert!(outer.encloses(&inner));
    assert!(!outer.encloses(&outside));
    assert!(!inner.encloses(&outer));
}

#[test]
fn percent_region_computes_correctly() {
    // Arrange / Act
    let region = Region::percent(50, 0, 50, 100, 80, 24);

    // Assert
    assert_eq!(region.col, 40);
    assert_eq!(region.row, 0);
    assert_eq!(region.width, 40);
    assert_eq!(region.height, 24);
}

#[test]
fn quadrants_and_panels_split_odd_dimensions() {
    // Arrange
    let cols = 81;
    let rows = 25;

    // Act
    let top_left = Region::top_left(cols, rows);
    let left_panel = Region::left_panel(cols, rows);
    let right_panel = Region::right_panel(cols, rows);

    // Assert — the right half takes the extra column.
    assert_eq!(top_left, Region::new(0, 0, 40, 12));
    assert_eq!(left_panel, Region::new(0, 0, 40, 25));
    assert_eq!(right_panel, Region::new(40, 0, 41, 25));
}

#[test]
fn top_right_covers_right_half() {
    // Arrange / Act
    let region = Region::top_right(80, 24);

    // Assert
    assert_eq!(region.col, 40);
    assert_eq!(region.row, 0);
    assert_eq!(region.width, 40);
    assert_eq!(region.height, 12);
}
