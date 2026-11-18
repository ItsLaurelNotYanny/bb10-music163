#ifndef LOCALTLSBRIDGE_H
#define LOCALTLSBRIDGE_H

#include <QByteArray>
#include <QObject>
#include <QString>
#include <QtNetwork/QTcpServer>
#include <QtNetwork/QTcpSocket>
#include <openssl/bio.h>
#include <openssl/ssl.h>

class BridgeSession : public QObject {
    Q_OBJECT

public:
    BridgeSession(QTcpSocket *client,
                  const QString &remoteHost,
                  quint16 remotePort,
                  SSL_CTX *sslCtx,
                  bool caLoaded,
                  QObject *parent = 0);
    ~BridgeSession();

private slots:
    void onClientReadyRead();
    void onClientDisconnected();
    void onUpstreamConnected();
    void onUpstreamReadyRead();
    void onUpstreamDisconnected();
    void onUpstreamError(QAbstractSocket::SocketError err);

private:
    void pumpTls();
    void flushWbioToUpstream();
    void sendErrorAndClose(const QString &msg);
    void shutdown();

    QTcpSocket *m_client;
    QTcpSocket *m_upstream;
    SSL *m_ssl;
    BIO *m_rbio;
    BIO *m_wbio;
    QByteArray m_pendingToUpstream;
    bool m_caLoaded;
    bool m_handshakeDone;
    bool m_sentUpstreamBytes;
    bool m_closed;
};

class LocalTlsBridge : public QObject {
    Q_OBJECT

public:
    explicit LocalTlsBridge(QObject *parent = 0);
    ~LocalTlsBridge();

    bool isListening() const;
    quint16 localPort() const;
    QString bridgeUrlFor(const QString &targetUrl);

private slots:
    void onNewConnection();

private:
    QTcpServer m_server;
    SSL_CTX *m_sslCtx;
    bool m_caLoaded;
    QString m_remoteHost;
    quint16 m_remotePort;
};

#endif
