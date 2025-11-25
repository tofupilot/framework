import time


def run_test(ui, log):
    log.info("Starting electrical test...")
    ui.status_text = "Initializing test equipment..."
    ui.progress_bar = 0
    time.sleep(1)

    ui.status_text = "Applying voltage..."
    ui.progress_bar = 25
    time.sleep(1)

    ui.status_text = "Measuring current..."
    ui.progress_bar = 50
    time.sleep(1)

    ui.status_text = "Validating parameters..."
    ui.progress_bar = 75
    time.sleep(1)

    ui.status_text = "Test complete!"
    ui.progress_bar = 100
    log.info("Electrical test completed successfully")
