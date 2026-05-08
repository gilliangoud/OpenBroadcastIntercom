#import <AVFoundation/AVFoundation.h>
#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

static NSMutableArray *intercom_audio_observers;
static BOOL intercom_audio_session_configured = NO;
static BOOL intercom_audio_session_configuring = NO;

static void intercom_write_error(char *buffer, size_t buffer_len, NSString *message) {
  if (buffer == NULL || buffer_len == 0) {
    return;
  }
  const char *text = message == nil ? "iOS audio session setup failed" : [message UTF8String];
  if (text == NULL) {
    text = "iOS audio session setup failed";
  }
  snprintf(buffer, buffer_len, "%s", text);
}

static NSString *intercom_error_text(NSString *prefix, NSError *error) {
  if (error == nil) {
    return prefix;
  }
  return [NSString stringWithFormat:@"%@: %@", prefix, error.localizedDescription];
}

static int intercom_request_audio_permission(char *error_buffer, size_t error_buffer_len) {
  AVAuthorizationStatus permission = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
  __block BOOL granted = permission == AVAuthorizationStatusAuthorized;
  if (permission == AVAuthorizationStatusNotDetermined) {
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL allowed) {
      granted = allowed;
      dispatch_semaphore_signal(semaphore);
    }];
    dispatch_semaphore_wait(semaphore, DISPATCH_TIME_FOREVER);
  }
  if (!granted) {
    intercom_write_error(error_buffer, error_buffer_len, @"Microphone permission denied. Enable microphone access in iOS Settings and start the client again.");
    return 1;
  }
  return 0;
}

static AVAudioSessionCategoryOptions intercom_audio_session_options(void) {
  AVAudioSessionCategoryOptions options =
      AVAudioSessionCategoryOptionDefaultToSpeaker |
#if defined(__IPHONE_OS_VERSION_MAX_ALLOWED) && __IPHONE_OS_VERSION_MAX_ALLOWED >= 260000
      AVAudioSessionCategoryOptionAllowBluetoothHFP;
#else
      AVAudioSessionCategoryOptionAllowBluetooth;
#endif
  if (@available(iOS 10.0, *)) {
    options |= AVAudioSessionCategoryOptionAllowBluetoothA2DP;
  }
  return options;
}

static BOOL intercom_audio_session_matches(AVAudioSession *session, AVAudioSessionCategoryOptions options) {
  return [session.category isEqualToString:AVAudioSessionCategoryPlayAndRecord] &&
         [session.mode isEqualToString:AVAudioSessionModeVoiceChat] &&
         (session.categoryOptions & options) == options;
}

static int intercom_configure_audio_session(char *error_buffer, size_t error_buffer_len, bool request_permission, bool force_reconfigure) {
  if (request_permission) {
    int permission_result = intercom_request_audio_permission(error_buffer, error_buffer_len);
    if (permission_result != 0) {
      return permission_result;
    }
  }

  AVAudioSession *session = [AVAudioSession sharedInstance];
  AVAudioSessionCategoryOptions options = intercom_audio_session_options();
  if (!force_reconfigure &&
      intercom_audio_session_configured &&
      intercom_audio_session_matches(session, options)) {
    return 0;
  }

  if (intercom_audio_session_configuring) {
    return 0;
  }
  intercom_audio_session_configuring = YES;

  NSError *error = nil;
  int result = 0;

  if (![session setCategory:AVAudioSessionCategoryPlayAndRecord
                   mode:AVAudioSessionModeVoiceChat
                options:options
                  error:&error]) {
    intercom_write_error(error_buffer, error_buffer_len, intercom_error_text(@"Set iOS PlayAndRecord audio category", error));
    result = 2;
    goto finish;
  }

  error = nil;
  if (![session setPreferredSampleRate:48000.0 error:&error]) {
    intercom_write_error(error_buffer, error_buffer_len, intercom_error_text(@"Set iOS preferred sample rate", error));
    result = 3;
    goto finish;
  }

  error = nil;
  if (![session setPreferredIOBufferDuration:0.02 error:&error]) {
    intercom_write_error(error_buffer, error_buffer_len, intercom_error_text(@"Set iOS audio buffer duration", error));
    result = 4;
    goto finish;
  }

  error = nil;
  if (![session setActive:YES error:&error]) {
    intercom_write_error(error_buffer, error_buffer_len, intercom_error_text(@"Activate iOS audio session", error));
    result = 5;
    goto finish;
  }

  intercom_audio_session_configured = YES;

finish:
  intercom_audio_session_configuring = NO;
  return result;
}

static void intercom_install_audio_observers(void) {
  static dispatch_once_t once;
  dispatch_once(&once, ^{
    intercom_audio_observers = [NSMutableArray array];
    NSNotificationCenter *center = [NSNotificationCenter defaultCenter];

    id route = [center addObserverForName:AVAudioSessionRouteChangeNotification
                                   object:nil
                                    queue:[NSOperationQueue mainQueue]
                               usingBlock:^(NSNotification *note) {
      NSNumber *reason = note.userInfo[AVAudioSessionRouteChangeReasonKey];
      if (reason != nil && reason.unsignedIntegerValue == AVAudioSessionRouteChangeReasonCategoryChange) {
        return;
      }
      intercom_configure_audio_session(NULL, 0, false, false);
    }];
    [intercom_audio_observers addObject:route];

    id interruption = [center addObserverForName:AVAudioSessionInterruptionNotification
                                          object:nil
                                           queue:[NSOperationQueue mainQueue]
                                      usingBlock:^(NSNotification *note) {
      NSNumber *type = note.userInfo[AVAudioSessionInterruptionTypeKey];
      if (type != nil && type.unsignedIntegerValue == AVAudioSessionInterruptionTypeEnded) {
        intercom_configure_audio_session(NULL, 0, false, true);
      }
    }];
    [intercom_audio_observers addObject:interruption];

    id reset = [center addObserverForName:AVAudioSessionMediaServicesWereResetNotification
                                   object:nil
                                    queue:[NSOperationQueue mainQueue]
                               usingBlock:^(__unused NSNotification *note) {
      intercom_audio_session_configured = NO;
      intercom_configure_audio_session(NULL, 0, false, true);
    }];
    [intercom_audio_observers addObject:reset];

    id active = [center addObserverForName:UIApplicationDidBecomeActiveNotification
                                    object:nil
                                     queue:[NSOperationQueue mainQueue]
                                usingBlock:^(__unused NSNotification *note) {
      intercom_configure_audio_session(NULL, 0, false, false);
    }];
    [intercom_audio_observers addObject:active];
  });
}

int intercom_ios_prepare_audio_session(char *error_buffer, size_t error_buffer_len) {
  int permission_result = intercom_request_audio_permission(error_buffer, error_buffer_len);
  if (permission_result != 0) {
    return permission_result;
  }

  __block int result = 0;
  if ([NSThread isMainThread]) {
    result = intercom_configure_audio_session(error_buffer, error_buffer_len, false, true);
    intercom_install_audio_observers();
    return result;
  }

  dispatch_sync(dispatch_get_main_queue(), ^{
    result = intercom_configure_audio_session(error_buffer, error_buffer_len, false, true);
    intercom_install_audio_observers();
  });
  return result;
}

static NSString *intercom_txt_value(NSDictionary<NSString *, NSData *> *txt, NSString *key) {
  NSData *data = txt[key];
  if (data == nil) {
    return nil;
  }
  NSString *value = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
  return value.length == 0 ? nil : value;
}

static NSString *intercom_ip_from_sockaddr(NSData *address_data) {
  const struct sockaddr *addr = (const struct sockaddr *)address_data.bytes;
  char buffer[INET6_ADDRSTRLEN] = {0};
  if (addr->sa_family == AF_INET) {
    const struct sockaddr_in *ipv4 = (const struct sockaddr_in *)addr;
    if (inet_ntop(AF_INET, &ipv4->sin_addr, buffer, sizeof(buffer)) != NULL) {
      return [NSString stringWithUTF8String:buffer];
    }
  } else if (addr->sa_family == AF_INET6) {
    const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)addr;
    if (inet_ntop(AF_INET6, &ipv6->sin6_addr, buffer, sizeof(buffer)) != NULL) {
      return [NSString stringWithUTF8String:buffer];
    }
  }
  return nil;
}

static NSString *intercom_host_for_url(NSString *host) {
  if ([host containsString:@":"] && ![host hasPrefix:@"["]) {
    return [NSString stringWithFormat:@"[%@]", host];
  }
  return host;
}

@interface IntercomBonjourBrowser : NSObject <NSNetServiceBrowserDelegate, NSNetServiceDelegate>
@property(nonatomic, strong) NSMutableArray<NSDictionary *> *results;
@property(nonatomic, strong) NSMutableArray<NSNetService *> *services;
@end

@implementation IntercomBonjourBrowser

- (instancetype)init {
  self = [super init];
  if (self != nil) {
    _results = [NSMutableArray array];
    _services = [NSMutableArray array];
  }
  return self;
}

- (void)netServiceBrowser:(NSNetServiceBrowser *)browser
           didFindService:(NSNetService *)service
               moreComing:(BOOL)moreComing {
  (void)browser;
  (void)moreComing;
  service.delegate = self;
  [self.services addObject:service];
  [service resolveWithTimeout:1.5];
}

- (void)netServiceDidResolveAddress:(NSNetService *)service {
  NSString *host = nil;
  for (NSData *address in service.addresses) {
    host = intercom_ip_from_sockaddr(address);
    if (host != nil && ![host containsString:@":"]) {
      break;
    }
  }
  if (host == nil) {
    host = service.hostName;
  }
  if (host == nil || host.length == 0) {
    return;
  }

  NSDictionary<NSString *, NSData *> *txt = [NSNetService dictionaryFromTXTRecordData:service.TXTRecordData ?: [NSData data]];
  NSString *audioPort = intercom_txt_value(txt, @"audio_port");
  NSString *adminPort = intercom_txt_value(txt, @"admin_port");
  NSString *auth = intercom_txt_value(txt, @"auth");
  NSString *version = intercom_txt_value(txt, @"version");
  NSString *displayName = intercom_txt_value(txt, @"name") ?: service.name;
  NSString *urlHost = intercom_host_for_url(host);
  NSString *server = [NSString stringWithFormat:@"%@:%@", urlHost, audioPort ?: [NSString stringWithFormat:@"%ld", (long)service.port]];
  NSString *control = [NSString stringWithFormat:@"ws://%@:%ld", urlHost, (long)service.port];
  NSString *admin = adminPort == nil ? @"" : [NSString stringWithFormat:@"http://%@:%@", urlHost, adminPort];
  NSString *identifier = [NSString stringWithFormat:@"%@|%@", displayName, control];

  NSMutableDictionary *result = [@{
    @"id": identifier,
    @"name": displayName ?: @"Intercom Suite",
    @"server": server,
    @"control": control,
    @"discovered": @YES
  } mutableCopy];
  if (admin.length > 0) {
    result[@"admin"] = admin;
  }
  if (auth.length > 0) {
    result[@"auth"] = auth;
  }
  if (version.length > 0) {
    result[@"version"] = version;
  }
  [self.results addObject:result];
}

@end

char *intercom_ios_browse_intercom_services(double timeout_seconds) {
  __block char *result = NULL;
  dispatch_block_t browse = ^{
    IntercomBonjourBrowser *delegate = [[IntercomBonjourBrowser alloc] init];
    NSNetServiceBrowser *browser = [[NSNetServiceBrowser alloc] init];
    browser.delegate = delegate;
    [browser searchForServicesOfType:@"_intercom-suite._tcp." inDomain:@"local."];

    NSTimeInterval scanTimeout = timeout_seconds < 0.25 ? 0.25 : timeout_seconds;
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:scanTimeout];
    while ([deadline timeIntervalSinceNow] > 0) {
      @autoreleasepool {
        [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                 beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.05]];
      }
    }
    [browser stop];

    NSData *json = [NSJSONSerialization dataWithJSONObject:delegate.results options:0 error:nil];
    if (json == nil) {
      result = strdup("[]");
      return;
    }
    NSString *jsonString = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
    result = strdup(jsonString.UTF8String ?: "[]");
  };

  browse();
  return result;
}

void intercom_ios_free_string(char *value) {
  free(value);
}
