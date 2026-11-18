#include "ShellConfig.h"
#include <QCoreApplication>
#include <bb/cascades/AbstractPane>
#include <bb/cascades/Application>
#include <bb/cascades/QmlDocument>

int main(int argc, char **argv) {
    bb::cascades::Application app(argc, argv);
    QCoreApplication::setOrganizationName("BB10Music163");
    QCoreApplication::setApplicationName("NetEaseMusic163");

    ShellConfig shellConfig;
    bb::cascades::QmlDocument *qml =
        bb::cascades::QmlDocument::create("asset:///main.qml").parent(&app);
    qml->setContextProperty("shellConfig", &shellConfig);

    bb::cascades::AbstractPane *root = qml->createRootObject<bb::cascades::AbstractPane>();
    if (!root) {
        return 1;
    }
    app.setScene(root);
    return bb::cascades::Application::exec();
}
