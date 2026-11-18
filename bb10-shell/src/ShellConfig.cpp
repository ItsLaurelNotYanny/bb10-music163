#include "ShellConfig.h"
#include <QSettings>

namespace {
const char *kSettingsKeyUrl = "gateway/url";
const char *kDefaultRemoteUrl = "https://gateway.example.com:16380/player";
const char *kDefaultLanUrl = "http://192.0.2.10:16381/player";
}

ShellConfig::ShellConfig(QObject *parent) : QObject(parent) {
    QSettings settings;
    QString saved = settings.value(kSettingsKeyUrl, QString::fromAscii(kDefaultRemoteUrl)).toString().trimmed();
    if (saved.isEmpty()) {
        saved = QString::fromAscii(kDefaultRemoteUrl);
    }
    m_gatewayUrl = saved;
}

QString ShellConfig::gatewayUrl() const {
    return m_gatewayUrl;
}

QString ShellConfig::webViewUrl() {
    return m_bridge.bridgeUrlFor(m_gatewayUrl);
}

void ShellConfig::setGatewayUrl(const QString &url) {
    QString trimmed = url.trimmed();
    if (trimmed.isEmpty()) {
        return;
    }
    if (!trimmed.startsWith("http://") && !trimmed.startsWith("https://")) {
        trimmed = QString::fromAscii("https://") + trimmed;
    }
    if (trimmed != m_gatewayUrl) {
        m_gatewayUrl = trimmed;
        QSettings settings;
        settings.setValue(kSettingsKeyUrl, m_gatewayUrl);
        settings.sync();
    }
    emit gatewayUrlChanged();
}

QString ShellConfig::defaultRemoteUrl() const {
    return QString::fromAscii(kDefaultRemoteUrl);
}

QString ShellConfig::defaultLanUrl() const {
    return QString::fromAscii(kDefaultLanUrl);
}
