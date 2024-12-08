pub fn is_number(s: &str) -> bool {
    s.len() > 0 && s.chars().all(|c| ('0' ..= '9').contains(&c))
}

pub fn avg(iter: impl Iterator<Item=f32>) -> Option<f32> {
    let mut count = 0;
    let mut sum = 0.;
    for i in iter {
        sum += i;
        count += 1;
    }
    if count > 0 {
        Some(sum / count as f32)
    } else {
        None
    }
}