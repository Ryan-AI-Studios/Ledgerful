use crate::downstream::work;

pub fn list_orders() -> &'static str {
    // slot
    work()
}

#[cfg(test)]
fn cfg_test_helper() {}
