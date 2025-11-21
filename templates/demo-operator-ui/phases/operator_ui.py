"""Operator UI phase examples"""

import time


def basic_inputs(test_api, ui):
    """Basic text input components"""
    test_api.log.info("Basic input components demo - Python phase complete")
    return "CONTINUE"


def selection_inputs(test_api, ui):
    """Selection input components (radio, dropdown, checklist, boolean)"""
    test_api.log.info("Selection components demo - Python phase complete")
    return "CONTINUE"


def slider_input(test_api, ui):
    """Slider input component"""
    test_api.log.info("Slider component demo - Python phase complete")
    return "CONTINUE"


def monitor_progress(test_api, ui):
    """Progress bar and output components with auto-update"""
    test_api.log.info("Starting progress monitoring")

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

        test_api.log.info(f"Progress: {i}% - {status_text}")
        test_api.ui.set_value("progress_bar", str(i))
        test_api.ui.set_value("label_output", status_text)
        test_api.ui.set_value("image_output", qr_url)
        time.sleep(0.8)

    test_api.log.info("Progress complete")
    return "CONTINUE"
