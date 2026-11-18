#include "LocalTlsBridge.h"
#include <QCoreApplication>
#include <QtNetwork/QHostAddress>
#include <QUrl>
#include <openssl/err.h>
#include <openssl/x509.h>
#include <openssl/x509v3.h>

namespace {
QString formatOpenSslError(SSL *ssl) {
    unsigned long code = ERR_get_error();
    char buf[256];
    if (code != 0) {
        ERR_error_string_n(code, buf, sizeof(buf));
        return QString::fromAscii(buf);
    }
    if (ssl) {
        long vr = SSL_get_verify_result(ssl);
        if (vr != X509_V_OK) {
            return QString("X509 verify error: %1").arg(X509_verify_cert_error_string(vr));
        }
    }
    return QString::fromAscii("unknown OpenSSL error");
}
}

BridgeSession::BridgeSession(QTcpSocket *client,
                             const QString &remoteHost,
                             quint16 remotePort,
                             SSL_CTX *sslCtx,
                             bool caLoaded,
                             QObject *parent)
    : QObject(parent),
      m_client(client),
      m_upstream(new QTcpSocket(this)),
      m_ssl(0),
      m_rbio(0),
      m_wbio(0),
      m_caLoaded(caLoaded),
      m_handshakeDone(false),
      m_sentUpstreamBytes(false),
      m_closed(false) {
    m_client->setParent(this);
    m_client->setSocketOption(QAbstractSocket::LowDelayOption, 1);

    m_upstream->setSocketOption(QAbstractSocket::LowDelayOption, 1);
    m_upstream->setSocketOption(QAbstractSocket::KeepAliveOption, 1);

    if (sslCtx) {
        m_ssl = SSL_new(sslCtx);
    }
    if (!m_ssl) {
        sendErrorAndClose("Failed to allocate OpenSSL SSL object");
        return;
    }
    m_rbio = BIO_new(BIO_s_mem());
    m_wbio = BIO_new(BIO_s_mem());
    SSL_set_bio(m_ssl, m_rbio, m_wbio);
    SSL_set_connect_state(m_ssl);

    QByteArray hostUtf8 = remoteHost.toUtf8();
    QHostAddress ipAddr;
    if (ipAddr.setAddress(remoteHost)) {
        X509_VERIFY_PARAM_set1_ip_asc(SSL_get0_param(m_ssl), hostUtf8.constData());
    } else {
        SSL_set_tlsext_host_name(m_ssl, hostUtf8.constData());
        SSL_set1_host(m_ssl, hostUtf8.constData());
    }

    connect(m_client, SIGNAL(readyRead()), this, SLOT(onClientReadyRead()));
    connect(m_client, SIGNAL(disconnected()), this, SLOT(onClientDisconnected()));
    connect(m_upstream, SIGNAL(connected()), this, SLOT(onUpstreamConnected()));
    connect(m_upstream, SIGNAL(readyRead()), this, SLOT(onUpstreamReadyRead()));
    connect(m_upstream, SIGNAL(disconnected()), this, SLOT(onUpstreamDisconnected()));
    connect(m_upstream, SIGNAL(error(QAbstractSocket::SocketError)),
            this, SLOT(onUpstreamError(QAbstractSocket::SocketError)));

    m_upstream->connectToHost(remoteHost, remotePort);
}

BridgeSession::~BridgeSession() {
    if (m_ssl) {
        SSL_free(m_ssl);
        m_ssl = 0;
    }
}

void BridgeSession::onClientReadyRead() {
    if (m_closed) {
        return;
    }
    QByteArray data = m_client->readAll();
    if (!data.isEmpty()) {
        m_pendingToUpstream.append(data);
        pumpTls();
    }
}

void BridgeSession::onUpstreamConnected() {
    if (m_closed) {
        return;
    }
    pumpTls();
}

void BridgeSession::onUpstreamReadyRead() {
    if (m_closed || !m_rbio) {
        return;
    }
    QByteArray enc = m_upstream->readAll();
    if (!enc.isEmpty()) {
        BIO_write(m_rbio, enc.constData(), enc.size());
        pumpTls();
    }
}

void BridgeSession::flushWbioToUpstream() {
    if (!m_wbio || m_upstream->state() != QAbstractSocket::ConnectedState) {
        return;
    }
    char outBuf[16384];
    while (BIO_ctrl_pending(m_wbio) > 0) {
        int n = BIO_read(m_wbio, outBuf, sizeof(outBuf));
        if (n <= 0) {
            break;
        }
        m_upstream->write(outBuf, n);
    }
}

void BridgeSession::pumpTls() {
    if (m_closed || !m_ssl || m_upstream->state() != QAbstractSocket::ConnectedState) {
        return;
    }

    if (!m_handshakeDone) {
        int rc = SSL_do_handshake(m_ssl);
        flushWbioToUpstream();
        if (rc == 1) {
            m_handshakeDone = true;
        } else {
            int err = SSL_get_error(m_ssl, rc);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
                return;
            }
            sendErrorAndClose(QString("OpenSSL handshake failed (ca=%1): %2")
                                  .arg(m_caLoaded ? 1 : 0)
                                  .arg(formatOpenSslError(m_ssl)));
            return;
        }
    }

    while (!m_pendingToUpstream.isEmpty()) {
        int n = SSL_write(m_ssl, m_pendingToUpstream.constData(), m_pendingToUpstream.size());
        flushWbioToUpstream();
        if (n > 0) {
            m_pendingToUpstream.remove(0, n);
        } else {
            int err = SSL_get_error(m_ssl, n);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
                break;
            }
            sendErrorAndClose(QString("OpenSSL write error: %1").arg(formatOpenSslError(m_ssl)));
            return;
        }
    }

    char plainBuf[16384];
    for (;;) {
        int r = SSL_read(m_ssl, plainBuf, sizeof(plainBuf));
        flushWbioToUpstream();
        if (r > 0) {
            m_sentUpstreamBytes = true;
            m_client->write(plainBuf, r);
        } else {
            int err = SSL_get_error(m_ssl, r);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE) {
                break;
            }
            if (err == SSL_ERROR_ZERO_RETURN) {
                shutdown();
                return;
            }
            if (!m_sentUpstreamBytes) {
                sendErrorAndClose(QString("OpenSSL read error: %1").arg(formatOpenSslError(m_ssl)));
            } else {
                shutdown();
            }
            return;
        }
    }
}

void BridgeSession::onClientDisconnected() {
    shutdown();
}

void BridgeSession::onUpstreamDisconnected() {
    if (!m_sentUpstreamBytes) {
        sendErrorAndClose(QString("Upstream disconnected before response (ca=%1, tcp=%2)")
                              .arg(m_caLoaded ? 1 : 0)
                              .arg(m_upstream->errorString()));
        return;
    }
    shutdown();
}

void BridgeSession::onUpstreamError(QAbstractSocket::SocketError) {
    if (!m_sentUpstreamBytes) {
        sendErrorAndClose(QString("Upstream TCP error (ca=%1): %2")
                              .arg(m_caLoaded ? 1 : 0)
                              .arg(m_upstream->errorString()));
        return;
    }
    shutdown();
}

void BridgeSession::sendErrorAndClose(const QString &msg) {
    if (m_closed) {
        return;
    }
    if (!m_sentUpstreamBytes && m_client->isWritable()) {
        QByteArray body = msg.toUtf8();
        QByteArray hdr = "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\nContent-Length: "
            + QByteArray::number(body.size()) + "\r\n\r\n";
        m_client->write(hdr);
        m_client->write(body);
        m_client->flush();
    }
    shutdown();
}

void BridgeSession::shutdown() {
    if (m_closed) {
        return;
    }
    m_closed = true;
    if (m_client->state() != QAbstractSocket::UnconnectedState) {
        m_client->disconnectFromHost();
    }
    if (m_upstream->state() != QAbstractSocket::UnconnectedState) {
        m_upstream->disconnectFromHost();
    }
    deleteLater();
}

LocalTlsBridge::LocalTlsBridge(QObject *parent)
    : QObject(parent),
      m_sslCtx(0),
      m_caLoaded(false),
      m_remotePort(443) {
    m_sslCtx = SSL_CTX_new(TLS_client_method());
    if (m_sslCtx) {
        SSL_CTX_set_min_proto_version(m_sslCtx, TLS1_2_VERSION);
        SSL_CTX_set_verify(m_sslCtx, SSL_VERIFY_PEER, 0);
        SSL_CTX_set_default_verify_paths(m_sslCtx);
        const QString caPath = QCoreApplication::applicationDirPath() + "/assets/ca.crt";
        if (SSL_CTX_load_verify_locations(m_sslCtx, caPath.toUtf8().constData(), 0) == 1) {
            m_caLoaded = true;
        }
    }

    connect(&m_server, SIGNAL(newConnection()), this, SLOT(onNewConnection()));
    m_server.listen(QHostAddress::LocalHost, 0);
}

LocalTlsBridge::~LocalTlsBridge() {
    if (m_sslCtx) {
        SSL_CTX_free(m_sslCtx);
        m_sslCtx = 0;
    }
}

bool LocalTlsBridge::isListening() const {
    return m_server.isListening();
}

quint16 LocalTlsBridge::localPort() const {
    return m_server.serverPort();
}

QString LocalTlsBridge::bridgeUrlFor(const QString &targetUrl) {
    QUrl parsed(targetUrl);
    if (parsed.scheme().toLower() != "https" || !m_server.isListening()) {
        return targetUrl;
    }
    m_remoteHost = parsed.host();
    m_remotePort = static_cast<quint16>(parsed.port(443));

    QString path = parsed.encodedPath();
    if (path.isEmpty()) {
        path = "/";
    }
    QString localUrl = QString("http://127.0.0.1:%1%2").arg(m_server.serverPort()).arg(path);
    if (parsed.hasQuery()) {
        localUrl += "?" + QString::fromAscii(parsed.encodedQuery());
    }
    return localUrl;
}

void LocalTlsBridge::onNewConnection() {
    while (m_server.hasPendingConnections()) {
        QTcpSocket *client = m_server.nextPendingConnection();
        if (!client) {
            continue;
        }
        if (m_remoteHost.isEmpty()) {
            client->disconnectFromHost();
            client->deleteLater();
            continue;
        }
        new BridgeSession(client, m_remoteHost, m_remotePort, m_sslCtx, m_caLoaded, this);
    }
}
