/* zd-spawnd — persistent root-context spawn daemon for Zdroid's
 * chroot adapter.
 *
 * One su elevation at boot (via Magisk module service.sh) lets us
 * skip Magisk's per-call su mediation queue (~200ms/spawn → ~5ms/spawn).
 *
 * Wire protocol: see PROTOCOL.md alongside this file.
 *
 * Lifecycle:
 *   1. service.sh execs us as root.
 *   2. We bind a filesystem Unix socket at SOCKET_PATH, mode 0660,
 *      owned by root:<zdroid_uid>.
 *   3. We accept() in a loop. Each accepted connection forks a worker.
 *   4. Worker reads request, validates peer creds, parses argv/env/cwd,
 *      receives stdin/stdout/stderr via SCM_RIGHTS, forks the actual
 *      target child, monitors, sends exit code back, closes connection.
 *   5. Daemon loops forever; service.sh restarts it if it dies.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pwd.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define SOCKET_PATH "/data/data/com.zdroid/files/run/zd-spawn"
#define SOCKET_DIR  "/data/data/com.zdroid/files/run"
#define LOG_PATH    "/data/adb/modules/zdroid-spawnd/zd-spawnd.log"

#define MAGIC      0x5A445350u  /* "ZDSP" little-endian */
#define VERSION    1u

#define FLAG_INTERACTIVE (1u << 0)

#define MAX_PROG_LEN  4096u
#define MAX_CWD_LEN   4096u
#define MAX_ARGV_LEN  65536u
#define MAX_ENVP_LEN  65536u
#define MAX_ARG_COUNT 1024u

/* The chroot rootfs path is hardcoded for now. Future: read from
 * /data/adb/modules/zdroid-spawnd/runtime.conf. */
static const char *g_chroot_root = "/data/local/nhsystem/kali-arm64";

/* ===== logging ============================================================ */

static FILE *g_log = NULL;

static void open_log(void) {
    g_log = fopen(LOG_PATH, "a");
    /* If we can't open the log, fall back to stderr. */
    if (!g_log) g_log = stderr;
    setvbuf(g_log, NULL, _IOLBF, 0);
}

static void logf(const char *level, const char *fmt, ...) {
    char tsbuf[64];
    time_t now = time(NULL);
    struct tm tm;
    localtime_r(&now, &tm);
    strftime(tsbuf, sizeof(tsbuf), "%Y-%m-%dT%H:%M:%S%z", &tm);

    fprintf(g_log, "%s %s ", tsbuf, level);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(g_log, fmt, ap);
    va_end(ap);
    fputc('\n', g_log);
}

/* ===== protocol I/O helpers =============================================== */

/* Read exactly `n` bytes or fail with error. Returns 0 on success,
 * -errno on error or short read (which is treated as a protocol
 * violation, not a transient EOF). */
static int read_full(int fd, void *buf, size_t n) {
    char *p = buf;
    while (n > 0) {
        ssize_t r = read(fd, p, n);
        if (r < 0) {
            if (errno == EINTR) continue;
            return -errno;
        }
        if (r == 0) return -EPIPE;
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int write_full(int fd, const void *buf, size_t n) {
    const char *p = buf;
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -errno;
        }
        p += w;
        n -= (size_t)w;
    }
    return 0;
}

/* Receive exactly 3 file descriptors via SCM_RIGHTS plus an arbitrary
 * payload byte (so the recvmsg has at least one byte of regular data —
 * Linux requires this for SCM_RIGHTS messages). Returns 0 on success
 * and stores fds in out_fds[0..2], or -errno on error. */
static int recv_fds3(int fd, int out_fds[3]) {
    struct msghdr msg;
    char dummy = 0;
    struct iovec iov = { .iov_base = &dummy, .iov_len = 1 };
    union {
        struct cmsghdr align;
        char buf[CMSG_SPACE(sizeof(int) * 3)];
    } cmsg_storage;

    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_storage.buf;
    msg.msg_controllen = sizeof(cmsg_storage.buf);

    ssize_t r = recvmsg(fd, &msg, MSG_CMSG_CLOEXEC);
    if (r < 0) return -errno;
    if (r == 0) return -EPIPE;

    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (!cmsg || cmsg->cmsg_level != SOL_SOCKET ||
        cmsg->cmsg_type != SCM_RIGHTS ||
        cmsg->cmsg_len != CMSG_LEN(sizeof(int) * 3)) {
        return -EBADMSG;
    }
    memcpy(out_fds, CMSG_DATA(cmsg), sizeof(int) * 3);
    return 0;
}

/* ===== request parsing ==================================================== */

struct request {
    uint32_t flags;
    char *prog;
    char *cwd;
    char **argv;  /* NULL-terminated, owned */
    char **envp;  /* NULL-terminated, owned */
    int stdio[3]; /* 0=stdin, 1=stdout, 2=stderr; owned, must close */
};

static void request_free(struct request *req) {
    free(req->prog);
    free(req->cwd);
    if (req->argv) {
        for (size_t i = 0; req->argv[i]; i++) free(req->argv[i]);
        free(req->argv);
    }
    if (req->envp) {
        for (size_t i = 0; req->envp[i]; i++) free(req->envp[i]);
        free(req->envp);
    }
    for (int i = 0; i < 3; i++) {
        if (req->stdio[i] >= 0) close(req->stdio[i]);
    }
}

/* Read one length-prefixed string. Caller frees `out`. */
static int read_lp_string(int fd, char **out, uint32_t max_len) {
    uint32_t len;
    int r = read_full(fd, &len, sizeof(len));
    if (r < 0) return r;
    if (len > max_len) return -EMSGSIZE;
    char *buf = malloc((size_t)len + 1);
    if (!buf) return -ENOMEM;
    if (len > 0) {
        r = read_full(fd, buf, len);
        if (r < 0) { free(buf); return r; }
    }
    buf[len] = '\0';
    *out = buf;
    return 0;
}

static int parse_request(int conn, struct request *req) {
    memset(req, 0, sizeof(*req));
    req->stdio[0] = req->stdio[1] = req->stdio[2] = -1;

    /* Header. */
    uint32_t header[7];
    int r = read_full(conn, header, sizeof(header));
    if (r < 0) { logf("WARN", "short header: %s", strerror(-r)); return r; }

    if (header[0] != MAGIC)   return -EBADMSG;
    if (header[1] != VERSION) return -EPROTONOSUPPORT;
    req->flags  = header[2];
    uint32_t prog_len = header[3];
    uint32_t cwd_len  = header[4];
    uint32_t argc     = header[5];
    uint32_t envc     = header[6];

    if (prog_len == 0 || prog_len > MAX_PROG_LEN) return -EMSGSIZE;
    if (cwd_len > MAX_CWD_LEN)                     return -EMSGSIZE;
    if (argc > MAX_ARG_COUNT || envc > MAX_ARG_COUNT) return -EMSGSIZE;

    /* prog. */
    req->prog = malloc((size_t)prog_len + 1);
    if (!req->prog) return -ENOMEM;
    r = read_full(conn, req->prog, prog_len);
    if (r < 0) return r;
    req->prog[prog_len] = '\0';

    /* cwd. */
    req->cwd = malloc((size_t)cwd_len + 1);
    if (!req->cwd) return -ENOMEM;
    if (cwd_len > 0) {
        r = read_full(conn, req->cwd, cwd_len);
        if (r < 0) return r;
    }
    req->cwd[cwd_len] = '\0';

    /* argv: argc entries, plus argv[0] = prog (we synthesize). */
    req->argv = calloc((size_t)argc + 2, sizeof(char *));
    if (!req->argv) return -ENOMEM;
    req->argv[0] = strdup(req->prog);
    if (!req->argv[0]) return -ENOMEM;
    for (uint32_t i = 0; i < argc; i++) {
        r = read_lp_string(conn, &req->argv[i + 1], MAX_ARGV_LEN);
        if (r < 0) return r;
    }

    /* envp. */
    req->envp = calloc((size_t)envc + 1, sizeof(char *));
    if (!req->envp) return -ENOMEM;
    for (uint32_t i = 0; i < envc; i++) {
        r = read_lp_string(conn, &req->envp[i], MAX_ENVP_LEN);
        if (r < 0) return r;
    }

    /* stdio fds via SCM_RIGHTS. */
    r = recv_fds3(conn, req->stdio);
    if (r < 0) {
        logf("WARN", "recv_fds3: %s", strerror(-r));
        return r;
    }

    return 0;
}

/* ===== response =========================================================== */

static int send_spawned(int conn, int32_t status, uint32_t child_pid) {
    uint32_t resp[4] = { MAGIC, VERSION, (uint32_t)status, child_pid };
    return write_full(conn, resp, sizeof(resp));
}

static int send_exited(int conn, int32_t exit_code) {
    uint32_t resp[3] = { MAGIC, VERSION, (uint32_t)exit_code };
    return write_full(conn, resp, sizeof(resp));
}

/* ===== child execution ==================================================== */

/* Fork the actual target child, chroot in, dup stdio, exec.
 * On return:
 *   ret >= 0: child PID (in parent), child running.
 *   ret < 0: -errno; no child.
 */
static int spawn_child(struct request *req) {
    pid_t pid = fork();
    if (pid < 0) return -errno;

    if (pid == 0) {
        /* Child. Note: error-path uses _exit() not exit(); avoids
         * flushing the parent's buffered I/O. */

        /* Replace stdio. */
        for (int i = 0; i < 3; i++) {
            if (dup2(req->stdio[i], i) < 0) _exit(127);
        }
        /* Close received-side fds (now duped). */
        for (int i = 0; i < 3; i++) {
            if (req->stdio[i] != i) close(req->stdio[i]);
        }

        /* Chroot. Requires CAP_SYS_CHROOT, which the daemon has by
         * virtue of being launched by Magisk's service.sh as root. */
        if (chroot(g_chroot_root) < 0) _exit(127);

        /* CWD inside chroot. If translation went wrong (cwd doesn't
         * exist), fall back to / so the spawn at least starts. */
        if (req->cwd[0] != '\0') {
            if (chdir(req->cwd) < 0) {
                if (chdir("/") < 0) _exit(127);
            }
        } else {
            if (chdir("/") < 0) _exit(127);
        }

        /* For interactive spawns: become session leader so the inner
         * shell can do job control. The pty (which the client passed
         * as stdio) becomes our controlling terminal. */
        if (req->flags & FLAG_INTERACTIVE) {
            if (setsid() < 0) {
                /* Already a session leader is fine. Other errors are
                 * non-fatal here; bash will still run, just without
                 * job control. */
            }
            /* TIOCSCTTY with force=1 to claim the pty even if some
             * other session lost track of it. Requires CAP_SYS_ADMIN
             * which we have. */
            ioctl(0, TIOCSCTTY, 1);
        }

        /* Exec. envp is our request's, argv is too. */
        execvpe(req->prog, req->argv, req->envp);
        _exit(127);
    }

    /* Parent: we don't need the stdio fds anymore. */
    for (int i = 0; i < 3; i++) {
        if (req->stdio[i] >= 0) {
            close(req->stdio[i]);
            req->stdio[i] = -1;
        }
    }

    return (int)pid;
}

/* ===== peer credential check ============================================== */

/* Verify the connecting peer's UID matches Zdroid's UID. Mode 0660 on
 * the socket already gates this filesystem-wise; SO_PEERCRED is
 * defense in depth (and catches misconfigured installs that loosen
 * the perms). */
static int check_peer_creds(int conn, uid_t expected_uid) {
    struct ucred cred;
    socklen_t len = sizeof(cred);
    if (getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &len) < 0) {
        logf("WARN", "SO_PEERCRED failed: %s", strerror(errno));
        return -errno;
    }
    if (cred.uid != expected_uid && cred.uid != 0) {
        logf("WARN", "rejecting connection from uid %u (expected %u)",
             cred.uid, expected_uid);
        return -EACCES;
    }
    return 0;
}

/* ===== connection handling ================================================ */

static void handle_connection(int conn, uid_t expected_uid) {
    if (check_peer_creds(conn, expected_uid) < 0) {
        close(conn);
        return;
    }

    struct request req;
    int r = parse_request(conn, &req);
    if (r < 0) {
        send_spawned(conn, r, 0);
        request_free(&req);
        close(conn);
        return;
    }

    int pid = spawn_child(&req);
    if (pid < 0) {
        logf("ERROR", "spawn '%s' failed: %s", req.prog, strerror(-pid));
        send_spawned(conn, pid, 0);
        request_free(&req);
        close(conn);
        return;
    }

    /* Reply that the spawn succeeded. */
    if (send_spawned(conn, 0, (uint32_t)pid) < 0) {
        kill(pid, SIGKILL);
        waitpid(pid, NULL, 0);
        request_free(&req);
        close(conn);
        return;
    }

    /* Wait for the child. */
    int status;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno != EINTR) {
            logf("ERROR", "waitpid(%d): %s", pid, strerror(errno));
            send_exited(conn, -1);
            request_free(&req);
            close(conn);
            return;
        }
    }

    int32_t exit_code;
    if (WIFEXITED(status))         exit_code = WEXITSTATUS(status);
    else if (WIFSIGNALED(status))  exit_code = -WTERMSIG(status);
    else                            exit_code = -1;

    send_exited(conn, exit_code);
    request_free(&req);
    close(conn);
}

/* ===== main loop ========================================================== */

static int setup_socket(uid_t zdroid_uid) {
    /* Ensure the socket dir exists with the right ownership. */
    if (mkdir(SOCKET_DIR, 0770) < 0 && errno != EEXIST) {
        logf("ERROR", "mkdir %s: %s", SOCKET_DIR, strerror(errno));
        return -1;
    }
    if (chown(SOCKET_DIR, zdroid_uid, zdroid_uid) < 0 && errno != EPERM) {
        /* Not fatal if EPERM (already owned correctly). */
        logf("WARN", "chown %s: %s", SOCKET_DIR, strerror(errno));
    }

    /* Remove any stale socket from a previous instance. */
    unlink(SOCKET_PATH);

    int sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (sock < 0) {
        logf("ERROR", "socket: %s", strerror(errno));
        return -1;
    }

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, SOCKET_PATH, sizeof(addr.sun_path) - 1);

    if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        logf("ERROR", "bind %s: %s", SOCKET_PATH, strerror(errno));
        close(sock);
        return -1;
    }

    /* Mode 0660 owner=root group=zdroid_uid: only root and Zdroid can
     * connect. */
    if (chmod(SOCKET_PATH, 0660) < 0) {
        logf("WARN", "chmod %s: %s", SOCKET_PATH, strerror(errno));
    }
    if (chown(SOCKET_PATH, 0, zdroid_uid) < 0) {
        logf("WARN", "chown %s: %s", SOCKET_PATH, strerror(errno));
    }

    if (listen(sock, 32) < 0) {
        logf("ERROR", "listen: %s", strerror(errno));
        close(sock);
        return -1;
    }

    return sock;
}

/* SIGCHLD handler: reap zombies from forked workers. */
static void on_sigchld(int sig) {
    (void)sig;
    while (waitpid(-1, NULL, WNOHANG) > 0) {}
}

int main(int argc, char **argv) {
    open_log();
    logf("INFO", "zd-spawnd starting (pid=%d)", getpid());

    /* Zdroid's app uid. Resolved from package name → uid. For now
     * accept it on the command line so we can test without
     * PackageManager. */
    if (argc < 2) {
        fprintf(stderr, "usage: zd-spawnd <zdroid-uid>\n");
        return 2;
    }
    uid_t zdroid_uid = (uid_t)atoi(argv[1]);
    logf("INFO", "zdroid_uid = %u", zdroid_uid);

    /* Reap zombies from forked workers without blocking accept(). */
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_sigchld;
    sa.sa_flags = SA_RESTART | SA_NOCLDSTOP;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGCHLD, &sa, NULL);

    /* Ignore SIGPIPE — write_full already returns -EPIPE on dead
     * peers and the worker handles that gracefully. */
    signal(SIGPIPE, SIG_IGN);

    int listen_fd = setup_socket(zdroid_uid);
    if (listen_fd < 0) return 1;
    logf("INFO", "listening on %s", SOCKET_PATH);

    for (;;) {
        int conn = accept4(listen_fd, NULL, NULL, SOCK_CLOEXEC);
        if (conn < 0) {
            if (errno == EINTR) continue;
            logf("ERROR", "accept: %s", strerror(errno));
            continue;
        }

        pid_t pid = fork();
        if (pid < 0) {
            logf("ERROR", "fork worker: %s", strerror(errno));
            send_spawned(conn, -errno, 0);
            close(conn);
            continue;
        }
        if (pid == 0) {
            /* Worker: close the listen fd, handle this one connection,
             * then exit. */
            close(listen_fd);
            handle_connection(conn, zdroid_uid);
            _exit(0);
        }
        /* Parent: don't keep a copy of the connection fd. */
        close(conn);
    }

    return 0;
}
