#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#include <sys/stat.h>
#include <stdint.h>

extern void macsweep_log(int error, const char *message);
#include <string.h>

// Return a malloc-owned string to Rust. A null result from the panel means Cancel.
char *macsweep_pick_path(int directory, const char *start) {
    @autoreleasepool {
        NSOpenPanel *panel = [NSOpenPanel openPanel];
        panel.canChooseFiles = !directory;
        panel.canChooseDirectories = directory;
        panel.allowsMultipleSelection = NO;
        panel.resolvesAliases = NO;
        panel.canDownloadUbiquitousContents = NO;
        panel.canResolveUbiquitousConflicts = NO;
        panel.treatsFilePackagesAsDirectories = NO;
        panel.title = directory ? @"Choose a folder or volume" : @"Choose an iCloud Drive file";
        panel.prompt = directory ? @"Map Folder" : @"Check Download";
        if (start) {
            NSURL *initial = [NSURL fileURLWithFileSystemRepresentation:start isDirectory:YES relativeToURL:nil];
            if ([[NSFileManager defaultManager] fileExistsAtPath:initial.path]) panel.directoryURL = initial;
        }
        if ([panel runModal] != NSModalResponseOK || !panel.URL.fileSystemRepresentation) return NULL;
        return strdup(panel.URL.fileSystemRepresentation);
    }
}

static char *detail(NSError *error, NSString *fallback) {
    if (!error) return strdup(fallback.UTF8String);
    NSString *code = [NSString stringWithFormat:@"%@ %ld", error.domain, (long)error.code];
    NSString *event = [@"iCloud: Foundation returned " stringByAppendingString:code];
    macsweep_log(1, event.UTF8String);
    NSString *message = [NSString stringWithFormat:@"%@ (%@)", error.localizedDescription, code];
    return strdup(message.UTF8String);
}

// Keep this layout in sync with CloudStatus in icloud.rs. A missing flag stays false.
typedef struct {
    uint8_t inCloud, uploaded, localCurrent, idle, conflictFree, purgeable, errorFree;
} CloudStatus;

int macsweep_cloud_state(const char *path, CloudStatus *status, char **errorText) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil];
        NSError *error = nil;
        NSNumber *ubiquitous = nil;
        if (![url getResourceValue:&ubiquitous forKey:NSURLIsUbiquitousItemKey error:&error]) {
            *errorText = detail(error, @"macOS could not check this file.");
            return 0;
        }
        status->inCloud = ubiquitous.boolValue;
        if (!status->inCloud) return 1;

        NSArray *keys = @[NSURLUbiquitousItemIsUploadedKey, NSURLUbiquitousItemIsUploadingKey,
            NSURLUbiquitousItemIsDownloadingKey, NSURLUbiquitousItemDownloadingStatusKey,
            NSURLUbiquitousItemHasUnresolvedConflictsKey, NSURLUbiquitousItemUploadingErrorKey,
            NSURLUbiquitousItemDownloadingErrorKey, NSURLIsPurgeableKey];
        NSDictionary *values = [url resourceValuesForKeys:keys error:&error];
        if (!values) {
            *errorText = detail(error, @"iCloud sync status is unavailable.");
            return 0;
        }
        status->uploaded = [values[NSURLUbiquitousItemIsUploadedKey] isEqual:@YES];
        status->localCurrent = [values[NSURLUbiquitousItemDownloadingStatusKey] isEqual:NSURLUbiquitousItemDownloadingStatusCurrent];
        status->idle = [values[NSURLUbiquitousItemIsUploadingKey] isEqual:@NO] && [values[NSURLUbiquitousItemIsDownloadingKey] isEqual:@NO];
        status->conflictFree = [values[NSURLUbiquitousItemHasUnresolvedConflictsKey] isEqual:@NO];
        status->purgeable = [values[NSURLIsPurgeableKey] isEqual:@YES];
        status->errorFree = ![values[NSURLUbiquitousItemUploadingErrorKey] isKindOfClass:NSError.class] &&
                            ![values[NSURLUbiquitousItemDownloadingErrorKey] isKindOfClass:NSError.class];
        return 1;
    }
}

// Resolve a file reference to pin the selected file's identity across renames.
// The Foundation eviction API coordinates its own operation and keeps the cloud original.
int macsweep_evict(const char *path, uint64_t device, uint64_t inode, int64_t size,
                   int64_t seconds, int64_t nanos, char **errorText) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil];
        NSURL *ref = url.fileReferenceURL;
        NSURL *currentURL = ref.filePathURL;
        const char *current = currentURL.fileSystemRepresentation;
        struct stat st, selected;
        if (!current || lstat(path, &selected) != 0 || lstat(current, &st) != 0 ||
            selected.st_ino != st.st_ino || selected.st_dev != st.st_dev ||
            !S_ISREG(st.st_mode) || st.st_nlink != 1 || (uint64_t)st.st_dev != device ||
            st.st_ino != inode || st.st_size != size || st.st_mtimespec.tv_sec != seconds ||
            st.st_mtimespec.tv_nsec != nanos) {
            *errorText = detail(nil, @"The file changed. Select it again.");
            return 0;
        }
        NSError *error = nil;
        if (![[NSFileManager defaultManager] evictUbiquitousItemAtURL:ref error:&error]) {
            *errorText = detail(error, @"macOS could not remove this download.");
            return 0;
        }
        return 1;
    }
}
