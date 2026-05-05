#import <AVFoundation/AVFoundation.h>

int intercom_macos_microphone_mode_supported(void) {
    if (@available(macOS 12.0, *)) {
        return 1;
    }
    return 0;
}

int intercom_macos_preferred_microphone_mode(void) {
    if (@available(macOS 12.0, *)) {
        return (int)[AVCaptureDevice preferredMicrophoneMode];
    }
    return -1;
}

int intercom_macos_active_microphone_mode(void) {
    if (@available(macOS 12.0, *)) {
        return (int)[AVCaptureDevice activeMicrophoneMode];
    }
    return -1;
}

void intercom_macos_show_microphone_modes_ui(void) {
    if (@available(macOS 12.0, *)) {
        [AVCaptureDevice showSystemUserInterface:AVCaptureSystemUserInterfaceMicrophoneModes];
    }
}
