#include <AudioToolbox/AudioToolbox.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define INTERCOM_IOS_INPUT_CAPACITY_FRAMES 8192

typedef void (*intercom_ios_voice_input_callback)(void *userdata, const float *samples,
                                                  size_t sample_count);

typedef struct IntercomIosVoiceInput {
  AudioUnit audio_unit;
  intercom_ios_voice_input_callback callback;
  void *userdata;
  float *input_buffer;
  UInt32 input_capacity;
  bool initialized;
  bool started;
} IntercomIosVoiceInput;

static void intercom_ios_voice_set_error(char *buffer, size_t buffer_len, const char *operation,
                                         OSStatus status) {
  if (buffer == NULL || buffer_len == 0) {
    return;
  }
  snprintf(buffer, buffer_len, "%s failed (%d)", operation, (int)status);
}

static void intercom_ios_voice_set_text_error(char *buffer, size_t buffer_len, const char *message) {
  if (buffer == NULL || buffer_len == 0) {
    return;
  }
  snprintf(buffer, buffer_len, "%s", message);
}

static void intercom_ios_voice_cleanup(IntercomIosVoiceInput *input) {
  if (input == NULL) {
    return;
  }
  if (input->audio_unit != NULL) {
    if (input->started) {
      AudioOutputUnitStop(input->audio_unit);
      input->started = false;
    }
    if (input->initialized) {
      AudioUnitUninitialize(input->audio_unit);
      input->initialized = false;
    }
    AudioComponentInstanceDispose(input->audio_unit);
    input->audio_unit = NULL;
  }
  free(input->input_buffer);
  input->input_buffer = NULL;
  free(input);
}

static AudioStreamBasicDescription intercom_ios_voice_stream_format(void) {
  AudioStreamBasicDescription format;
  memset(&format, 0, sizeof(format));
  format.mSampleRate = 48000.0;
  format.mFormatID = kAudioFormatLinearPCM;
  format.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
  format.mBytesPerPacket = sizeof(float);
  format.mFramesPerPacket = 1;
  format.mBytesPerFrame = sizeof(float);
  format.mChannelsPerFrame = 1;
  format.mBitsPerChannel = sizeof(float) * 8;
  return format;
}

static OSStatus intercom_ios_voice_input_proc(void *ref_con,
                                              AudioUnitRenderActionFlags *action_flags,
                                              const AudioTimeStamp *time_stamp,
                                              UInt32 bus_number,
                                              UInt32 frame_count,
                                              AudioBufferList *io_data) {
  (void)bus_number;
  (void)io_data;
  IntercomIosVoiceInput *input = (IntercomIosVoiceInput *)ref_con;
  if (input == NULL || frame_count == 0) {
    return noErr;
  }
  if (frame_count > input->input_capacity) {
    return kAudio_ParamError;
  }

  AudioBufferList buffer_list;
  buffer_list.mNumberBuffers = 1;
  buffer_list.mBuffers[0].mNumberChannels = 1;
  buffer_list.mBuffers[0].mDataByteSize = frame_count * sizeof(float);
  buffer_list.mBuffers[0].mData = input->input_buffer;

  OSStatus status = AudioUnitRender(input->audio_unit, action_flags, time_stamp, 1, frame_count,
                                    &buffer_list);
  if (status != noErr) {
    return status;
  }

  if (input->callback != NULL) {
    input->callback(input->userdata, input->input_buffer, frame_count);
  }
  return noErr;
}

static OSStatus intercom_ios_voice_output_proc(void *ref_con,
                                               AudioUnitRenderActionFlags *action_flags,
                                               const AudioTimeStamp *time_stamp,
                                               UInt32 bus_number,
                                               UInt32 frame_count,
                                               AudioBufferList *io_data) {
  (void)ref_con;
  (void)action_flags;
  (void)time_stamp;
  (void)bus_number;
  (void)frame_count;
  if (io_data == NULL) {
    return noErr;
  }
  for (UInt32 index = 0; index < io_data->mNumberBuffers; index += 1) {
    if (io_data->mBuffers[index].mData != NULL) {
      memset(io_data->mBuffers[index].mData, 0, io_data->mBuffers[index].mDataByteSize);
    }
  }
  return noErr;
}

static OSStatus intercom_ios_voice_enable_io(AudioUnit audio_unit, AudioUnitScope scope,
                                             AudioUnitElement element, UInt32 enabled) {
  return AudioUnitSetProperty(audio_unit, kAudioOutputUnitProperty_EnableIO, scope, element,
                              &enabled, sizeof(enabled));
}

static void intercom_ios_voice_apply_processing_defaults(AudioUnit audio_unit) {
  UInt32 bypass = 0;
  AudioUnitSetProperty(audio_unit, kAUVoiceIOProperty_BypassVoiceProcessing,
                       kAudioUnitScope_Global, 0, &bypass, sizeof(bypass));

  UInt32 agc = 1;
  AudioUnitSetProperty(audio_unit, kAUVoiceIOProperty_VoiceProcessingEnableAGC,
                       kAudioUnitScope_Global, 0, &agc, sizeof(agc));
}

void *intercom_ios_voice_input_create(intercom_ios_voice_input_callback callback,
                                      void *userdata,
                                      char *error_buffer,
                                      size_t error_buffer_len) {
  IntercomIosVoiceInput *input = calloc(1, sizeof(IntercomIosVoiceInput));
  if (input == NULL) {
    intercom_ios_voice_set_text_error(error_buffer, error_buffer_len,
                                      "allocate iOS VoiceProcessingIO state failed");
    return NULL;
  }

  input->input_capacity = INTERCOM_IOS_INPUT_CAPACITY_FRAMES;
  input->input_buffer = calloc(input->input_capacity, sizeof(float));
  if (input->input_buffer == NULL) {
    intercom_ios_voice_set_text_error(error_buffer, error_buffer_len,
                                      "allocate iOS VoiceProcessingIO input buffer failed");
    intercom_ios_voice_cleanup(input);
    return NULL;
  }
  input->callback = callback;
  input->userdata = userdata;

  AudioComponentDescription description;
  memset(&description, 0, sizeof(description));
  description.componentType = kAudioUnitType_Output;
  description.componentSubType = kAudioUnitSubType_VoiceProcessingIO;
  description.componentManufacturer = kAudioUnitManufacturer_Apple;

  AudioComponent component = AudioComponentFindNext(NULL, &description);
  if (component == NULL) {
    intercom_ios_voice_set_text_error(error_buffer, error_buffer_len,
                                      "find iOS VoiceProcessingIO component failed");
    intercom_ios_voice_cleanup(input);
    return NULL;
  }

  OSStatus status = AudioComponentInstanceNew(component, &input->audio_unit);
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "create iOS VoiceProcessingIO audio unit", status);
    intercom_ios_voice_cleanup(input);
    return NULL;
  }

  status = intercom_ios_voice_enable_io(input->audio_unit, kAudioUnitScope_Input, 1, 1);
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "enable iOS VoiceProcessingIO input", status);
    intercom_ios_voice_cleanup(input);
    return NULL;
  }

  bool output_enabled =
      intercom_ios_voice_enable_io(input->audio_unit, kAudioUnitScope_Output, 0, 1) == noErr;

  AudioStreamBasicDescription format = intercom_ios_voice_stream_format();
  status = AudioUnitSetProperty(input->audio_unit, kAudioUnitProperty_StreamFormat,
                                kAudioUnitScope_Output, 1, &format, sizeof(format));
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "set iOS VoiceProcessingIO input format", status);
    intercom_ios_voice_cleanup(input);
    return NULL;
  }

  AURenderCallbackStruct input_callback;
  input_callback.inputProc = intercom_ios_voice_input_proc;
  input_callback.inputProcRefCon = input;
  status = AudioUnitSetProperty(input->audio_unit, kAudioOutputUnitProperty_SetInputCallback,
                                kAudioUnitScope_Global, 1, &input_callback,
                                sizeof(input_callback));
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "set iOS VoiceProcessingIO input callback", status);
    intercom_ios_voice_cleanup(input);
    return NULL;
  }

  if (output_enabled) {
    status = AudioUnitSetProperty(input->audio_unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Input, 0, &format, sizeof(format));
    if (status == noErr) {
      AURenderCallbackStruct output_callback;
      output_callback.inputProc = intercom_ios_voice_output_proc;
      output_callback.inputProcRefCon = input;
      status = AudioUnitSetProperty(input->audio_unit, kAudioUnitProperty_SetRenderCallback,
                                    kAudioUnitScope_Input, 0, &output_callback,
                                    sizeof(output_callback));
    }
    if (status != noErr) {
      intercom_ios_voice_enable_io(input->audio_unit, kAudioUnitScope_Output, 0, 0);
    }
  }

  intercom_ios_voice_apply_processing_defaults(input->audio_unit);

  status = AudioUnitInitialize(input->audio_unit);
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "initialize iOS VoiceProcessingIO audio unit", status);
    intercom_ios_voice_cleanup(input);
    return NULL;
  }
  input->initialized = true;

  return input;
}

int intercom_ios_voice_input_start(void *handle, char *error_buffer, size_t error_buffer_len) {
  IntercomIosVoiceInput *input = (IntercomIosVoiceInput *)handle;
  if (input == NULL) {
    intercom_ios_voice_set_text_error(error_buffer, error_buffer_len,
                                      "start iOS VoiceProcessingIO input with null handle");
    return 1;
  }
  if (input->started) {
    return 0;
  }

  OSStatus status = AudioOutputUnitStart(input->audio_unit);
  if (status != noErr) {
    intercom_ios_voice_set_error(error_buffer, error_buffer_len,
                                 "start iOS VoiceProcessingIO input", status);
    return 2;
  }
  input->started = true;
  return 0;
}

void intercom_ios_voice_input_stop(void *handle) {
  IntercomIosVoiceInput *input = (IntercomIosVoiceInput *)handle;
  if (input == NULL || !input->started) {
    return;
  }
  AudioOutputUnitStop(input->audio_unit);
  input->started = false;
}

void intercom_ios_voice_input_destroy(void *handle) {
  intercom_ios_voice_cleanup((IntercomIosVoiceInput *)handle);
}
