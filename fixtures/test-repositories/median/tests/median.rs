use median::median;

#[test]
fn an_odd_number_of_values_has_a_middle() {
    assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
}

#[test]
fn an_even_number_of_values_averages_the_middle_two() {
    assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
}

#[test]
fn no_values_has_no_median() {
    assert_eq!(median(&mut []), None);
}

#[test]
fn a_single_value_is_its_own_median() {
    assert_eq!(median(&mut [7.5]), Some(7.5));
}
