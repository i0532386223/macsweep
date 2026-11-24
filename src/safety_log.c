#include <dispatch/dispatch.h>
#include <os/log.h>

static os_log_t logger;
static dispatch_once_t once;

static void init_log(void *context) {
    (void)context;
    logger = os_log_create("me.i0532386223.macsweep", "safety");
}

void macsweep_log(int error, const char *message) {
    dispatch_once_f(&once, NULL, init_log);
    if (error) {
        os_log_error(logger, "%{public}s", message);
    } else {
        os_log_info(logger, "%{public}s", message);
    }
}
