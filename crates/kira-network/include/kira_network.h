#ifndef KIRA_NETWORK_H
#define KIRA_NETWORK_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* kira_network_poll states. */
#define KIRA_NETWORK_POLL_PENDING 0
#define KIRA_NETWORK_POLL_READY 1
#define KIRA_NETWORK_POLL_FAILED (-1)

/* Stable negative error codes returned by starts, polling, and results. */
#define KIRA_NETWORK_ERROR_RUNTIME_INIT (-100)
#define KIRA_NETWORK_ERROR_UNKNOWN_HANDLE (-101)
#define KIRA_NETWORK_ERROR_BIND (-102)
#define KIRA_NETWORK_ERROR_CONNECT (-103)
#define KIRA_NETWORK_ERROR_PROTOCOL (-104)
#define KIRA_NETWORK_ERROR_IO (-105)
#define KIRA_NETWORK_ERROR_NOT_READY (-106)
#define KIRA_NETWORK_ERROR_MISSING_CERTIFICATE (-107)
#define KIRA_NETWORK_ERROR_ID_EXHAUSTED (-108)
#define KIRA_NETWORK_ERROR_INVALID_URI (-109)
#define KIRA_NETWORK_ERROR_TIMEOUT (-110)
#define KIRA_NETWORK_ERROR_CANCELED (-111)
#define KIRA_NETWORK_ERROR_BODY_TOO_LARGE (-112)
#define KIRA_NETWORK_ERROR_DNS (-113)
#define KIRA_NETWORK_ERROR_HEADER (-114)
#define KIRA_NETWORK_ERROR_UNSUPPORTED (-115)
#define KIRA_NETWORK_ERROR_INVALID_CONFIG (-116)
#define KIRA_NETWORK_ERROR_ENCODING (-117)

/* Returned by the response readers at the end of a selection, and by
 * kira_network_response_select_header for a header the response does not
 * carry. */
#define KIRA_NETWORK_END_OF_SELECTION (-1)

/*
 * Start functions return a positive operation handle, or a negative error
 * code. A handle is polled until it is ready or failed, then released with
 * kira_network_close (or canceled with kira_network_cancel).
 */
int64_t kira_network_http1_server(void);
int64_t kira_network_http1_client(uint16_t port);
int64_t kira_network_http2_server(void);
int64_t kira_network_http2_client(uint16_t port);
int64_t kira_network_http3_server(void);
int64_t kira_network_http3_client(uint16_t port);
int64_t kira_network_websocket_server(void);
int64_t kira_network_websocket_client(uint16_t port);
int64_t kira_network_io_roundtrip(void);

/*
 * The HTTPS loopback server serves until it is cancelled rather than
 * completing: it has no single exchange to finish on. Its certificate is
 * published for kira_network_request_trust_loopback.
 */
int64_t kira_network_https_server(void);

/* Returns a bound server's port, or a negative error code. */
int64_t kira_network_server_port(int64_t handle);

/* Returns KIRA_NETWORK_POLL_* or a negative error code. */
int32_t kira_network_poll(int64_t handle);

/* Returns a completed operation value, or a negative error code. */
int64_t kira_network_result(int64_t handle);

/*
 * A request is assembled against a request handle, then sent. Every setter
 * answers 0 or a negative error code. kira_network_request_send consumes the
 * request handle and returns the operation handle that polls, results, reads
 * and cancels like any other.
 */
int64_t kira_network_request_new(const char *method, const char *url);
int64_t kira_network_request_header(int64_t request, const char *name,
                                    const char *value);
int64_t kira_network_request_body_text(int64_t request, const char *text);
int64_t kira_network_request_body_byte(int64_t request, int32_t byte);
int64_t kira_network_request_timeout_ms(int64_t request, int64_t milliseconds);
/* 1 selects HTTP/1.1, 2 selects HTTP/2. */
int64_t kira_network_request_version(int64_t request, int32_t version);
/* Trusts the certificate published by the loopback server bound to port. */
int64_t kira_network_request_trust_loopback(int64_t request, uint16_t port);
int64_t kira_network_request_send(int64_t request);
void kira_network_request_discard(int64_t request);

/*
 * A completed request's response is read through its operation handle. One
 * selection is current at a time — the body, or one header — and the reads
 * advance a cursor the operation owns.
 */
int64_t kira_network_response_status(int64_t handle);
int64_t kira_network_response_select_body(int64_t handle);
int64_t kira_network_response_select_header(int64_t handle, const char *name);
int64_t kira_network_response_length(int64_t handle);
int64_t kira_network_response_rewind(int64_t handle);
int64_t kira_network_response_read_byte(int64_t handle);
int64_t kira_network_response_read_scalar(int64_t handle);

/* Idempotently cancels and removes an operation. */
void kira_network_cancel(int64_t handle);

/* Compatibility alias for kira_network_cancel. */
void kira_network_close(int64_t handle);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* KIRA_NETWORK_H */
