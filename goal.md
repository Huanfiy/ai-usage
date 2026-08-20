需求：我要做一个各类 AI 使用量聚合统计的项目，整体架构分为前后端，为两个独立的程序
架构支持多宿主机聚合：
有一个前端看板，支持接收多后端数据上报，前端可部署在本机也可以部署到服务器

后端职责：自动化收集各类 AI 工具使用日志，元数据，可参考项目：
    https://github.com/vibe-cafe/vibe-usage
    https://github.com/nontracey/AIUsageStatistics
前端职责：解析处理后端上报的数据，进行数据分析与处理, 可参考：tmp/refer/uidemo.html

其他要求：
1. 项目部署要便利
2. 项目对宿主机环境不能用耦合型污染，自身需独立解耦
3. 注重项目性能