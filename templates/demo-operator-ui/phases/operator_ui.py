"""Operator UI phase examples"""

import time


def basic_inputs(run, ui):
    """Basic text input components"""
    run.log.info("Basic input components demo - Python phase complete")


def selection_inputs(run, ui):
    """Selection input components (radio, dropdown, checklist, boolean)"""
    run.log.info("Selection components demo - Python phase complete")


def slider_input(run, ui):
    """Slider input component"""
    run.log.info("Slider component demo - Python phase complete")


def monitor_progress(run, ui):
    """Progress bar and output components with auto-update"""
    run.log.info("Starting progress monitoring")

    stages = [
        ("Initializing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage1"),
        ("Loading data...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage2"),
        ("Processing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage3"),
        ("Validating...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage4"),
        ("Finalizing...", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Stage5"),
        ("Complete!", "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=Success"),
    ]

    for i in range(0, 101, 20):
        stage_index = min(i // 20, len(stages) - 1)
        status_text, qr_url = stages[stage_index]

        run.log.info(f"Progress: {i}% - {status_text}")
        run.ui.set_value("progress_bar", str(i))
        run.ui.set_value("label_output", status_text)
        run.ui.set_value("image_output", qr_url)
        time.sleep(0.8)

    run.log.info("Progress complete")
