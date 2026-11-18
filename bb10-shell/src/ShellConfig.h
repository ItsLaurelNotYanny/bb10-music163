#ifndef SHELLCONFIG_H
#define SHELLCONFIG_H

#include "LocalTlsBridge.h"
#include <QObject>
#include <QString>

class ShellConfig : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString gatewayUrl READ gatewayUrl WRITE setGatewayUrl NOTIFY gatewayUrlChanged)
    Q_PROPERTY(QString webViewUrl READ webViewUrl NOTIFY gatewayUrlChanged)

public:
    explicit ShellConfig(QObject *parent = 0);

    QString gatewayUrl() const;
    QString webViewUrl();

    Q_INVOKABLE void setGatewayUrl(const QString &url);
    Q_INVOKABLE QString defaultRemoteUrl() const;
    Q_INVOKABLE QString defaultLanUrl() const;

signals:
    void gatewayUrlChanged();

private:
    LocalTlsBridge m_bridge;
    QString m_gatewayUrl;
};

#endif
